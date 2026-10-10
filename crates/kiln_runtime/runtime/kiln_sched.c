/*
 * The Kiln task runtime: an M:N work-stealing scheduler, `Task<T>` handles,
 * channels, `select`, and scoped tasks.
 *
 * Tasks are stackful coroutines. Each worker thread runs a scheduler loop on
 * its own stack and switches into tasks; a task switches back when it blocks
 * (parks), finishes, or is woken elsewhere. A parked task is resumed by
 * whichever worker picks it up next, so tasks migrate between threads.
 *
 *   - Workers: one per CPU (`KILN_WORKERS` overrides). The main thread is
 *     worker 0; the others start on the first spawn.
 *   - Run queues: a queue per worker plus a global queue. An idle worker
 *     steals half of another worker's queue, then sleeps.
 *   - Parking follows Go's "unlock after switch" rule: a task that blocks on a
 *     lock-protected queue (channel, join, scope) asks the scheduler to release
 *     the lock only after the task's context is saved, so a waker can never
 *     resume a task that is still running.
 *   - Deadlock: when every worker is idle and nothing is runnable, every task
 *     is blocked forever; the program exits with Go's message.
 *   - Context switches: hand-written for x86-64 (System V) and AArch64 on
 *     Linux and macOS; Windows uses fibers.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <pthread.h>
#include <signal.h>
#include <sys/mman.h>
#include <unistd.h>
#endif

#ifdef _MSC_VER
#define KN_NORETURN __declspec(noreturn)
#define KN_NOINLINE __declspec(noinline)
#define KN_TLS __declspec(thread)
#else
#define KN_NORETURN __attribute__((noreturn))
#define KN_NOINLINE __attribute__((noinline))
#define KN_TLS __thread
#endif

KN_NORETURN void kn_panic_str(const char *msg);
void kn_free_env(void *env);

/* ----- platform: locks, atomics, threads --------------------------------- */

#ifdef _WIN32
typedef SRWLOCK kn_mutex;
typedef CONDITION_VARIABLE kn_cond;
static void mutex_init(kn_mutex *m) { InitializeSRWLock(m); }
static void mutex_lock(kn_mutex *m) { AcquireSRWLockExclusive(m); }
static void mutex_unlock(kn_mutex *m) { ReleaseSRWLockExclusive(m); }
static void cond_init(kn_cond *c) { InitializeConditionVariable(c); }
static void cond_wait(kn_cond *c, kn_mutex *m) { SleepConditionVariableSRW(c, m, INFINITE, 0); }
static void cond_signal(kn_cond *c) { WakeConditionVariable(c); }
static int64_t at_add(volatile int64_t *p, int64_t v) { return InterlockedExchangeAdd64((volatile LONG64 *)p, v) + v; }
static int64_t at_load(volatile int64_t *p) { return InterlockedCompareExchange64((volatile LONG64 *)p, 0, 0); }
static int at_cas32(volatile int32_t *p, int32_t old, int32_t nw) {
	return InterlockedCompareExchange((volatile LONG *)p, nw, old) == old;
}
#else
typedef pthread_mutex_t kn_mutex;
typedef pthread_cond_t kn_cond;
static void mutex_init(kn_mutex *m) { pthread_mutex_init(m, NULL); }
static void mutex_lock(kn_mutex *m) { pthread_mutex_lock(m); }
static void mutex_unlock(kn_mutex *m) { pthread_mutex_unlock(m); }
static void cond_init(kn_cond *c) { pthread_cond_init(c, NULL); }
static void cond_wait(kn_cond *c, kn_mutex *m) { pthread_cond_wait(c, m); }
static void cond_signal(kn_cond *c) { pthread_cond_signal(c); }
static int64_t at_add(volatile int64_t *p, int64_t v) { return __atomic_add_fetch(p, v, __ATOMIC_SEQ_CST); }
static int64_t at_load(volatile int64_t *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
static int at_cas32(volatile int32_t *p, int32_t old, int32_t nw) {
	return __atomic_compare_exchange_n(p, &old, nw, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
#endif

static void unlock_cb(void *m) { mutex_unlock((kn_mutex *)m); }

/* ----- tasks and workers ------------------------------------------------- */

/* Closure ABI used by generated code: `code(ret, env)`. */
typedef void (*KnCode)(void *ret, void *env);

typedef struct KnShared KnShared;
typedef struct KnScope KnScope;

typedef struct KnTask {
	void *sp;       /* saved stack pointer (POSIX) */
	void *fiber;    /* fiber (Windows) */
	char *stack;    /* stack mapping (POSIX) */
	KnCode code;
	void *env;
	int free_env;   /* env was allocated for this task */
	void *ret;      /* where the result goes */
	KnShared *shared;
	KnScope *scope;
	int64_t sel;    /* select: index of the case that completed */
	struct KnTask *next;
	char scratch[16];
} KnTask;

typedef struct KnWorker {
	kn_mutex lock;
	int64_t slice_start; /* when the current task was switched in (ns) */
	KnTask *head, *tail;
	volatile int64_t count;
	void *sched_sp;
	void *sched_fiber;
	KnTask *cur;
	void (*after)(void *);
	void *after_arg;
	uint64_t rng;
} KnWorker;

static KnWorker *workers;
static int64_t nworkers = 1;
static volatile int64_t nstarted = 1;
static volatile int32_t started_all;
static kn_mutex global_lock;
static KnTask *ghead, *gtail;
static volatile int64_t gcount;
static kn_mutex sched_lock;
static kn_cond sched_cond;
static volatile int64_t idle;
/* Tasks parked on something outside the scheduler (a timer, the network
 * poller): while any exist, all-workers-idle is not a deadlock. */
static volatile int64_t external;
static KN_TLS KnWorker *tls_worker;

/* Not inlined, and with a compiler barrier, so the thread-local is re-read
 * after every context switch (a task may resume on another thread). */
static KN_NOINLINE KnWorker *current_worker(void) {
#ifndef _MSC_VER
	__asm__ volatile("" ::: "memory");
#endif
	return tls_worker;
}

static KnTask *current_task(void) { return current_worker()->cur; }

static uint64_t next_rand(void) {
	KnWorker *w = current_worker();
	uint64_t x = w->rng;
	x ^= x << 13;
	x ^= x >> 7;
	x ^= x << 17;
	w->rng = x;
	return x;
}

/* ----- context switching ------------------------------------------------- */

/* Each task reserves 8 MiB of address space for its stack. Pages are only
 * committed when touched, so small tasks stay small. Overflowing it hits a
 * guard page (functions with big frames probe each page on entry) and the
 * program stops with a clear "stack overflow" message. */
#define KN_STACK_SIZE ((size_t)8 << 20)

static const char STACK_OVERFLOW_MSG[] =
	"fatal error: stack overflow (each goroutine has an 8 MiB stack)\n"
	"note: deep recursion or a large local array can cause this; keep big data on the heap (Vec, Box)\n";

void kn_task_start(KnTask *t);

#ifdef _WIN32

static VOID CALLBACK fiber_entry(LPVOID p) { kn_task_start((KnTask *)p); }

static int task_init_stack(KnTask *t) {
	t->fiber = CreateFiberEx(64 * 1024, KN_STACK_SIZE, FIBER_FLAG_FLOAT_SWITCH, fiber_entry, t);
	return t->fiber != NULL;
}

static void task_free_stack(KnTask *t) { DeleteFiber(t->fiber); }

static void switch_to_task(KnWorker *w, KnTask *t) {
	(void)w;
	SwitchToFiber(t->fiber);
}

static void switch_to_sched(KnTask *t) {
	(void)t;
	SwitchToFiber(current_worker()->sched_fiber);
}

static LONG CALLBACK overflow_handler(PEXCEPTION_POINTERS info) {
	if (info->ExceptionRecord->ExceptionCode == EXCEPTION_STACK_OVERFLOW) {
		DWORD n;
		WriteFile(GetStdHandle(STD_ERROR_HANDLE), STACK_OVERFLOW_MSG, (DWORD)(sizeof STACK_OVERFLOW_MSG - 1), &n, NULL);
		TerminateProcess(GetCurrentProcess(), 134);
	}
	return EXCEPTION_CONTINUE_SEARCH;
}

static volatile LONG overflow_installed;

static void worker_enter(KnWorker *w) {
	if (InterlockedExchange(&overflow_installed, 1) == 0)
		AddVectoredExceptionHandler(1, overflow_handler);
	w->sched_fiber = ConvertThreadToFiberEx(NULL, FIBER_FLAG_FLOAT_SWITCH);
}

#else

void kn_ctx_switch(void **save, void *to);
void kn_ctx_trampoline(void);

#if defined(__APPLE__)
#define KN_SYM(x) "_" #x
#define KN_FN_TYPE(x) ""
#else
#define KN_SYM(x) #x
#define KN_FN_TYPE(x) ".type " #x ", @function\n"
#endif

#if defined(__x86_64__)
/* Saves rbp, rbx, r12-r15, MXCSR and the x87 control word on the current
 * stack, stores the stack pointer in *save, and resumes the context at `to`. */
__asm__(
	".text\n"
	".globl " KN_SYM(kn_ctx_switch) "\n"
	KN_FN_TYPE(kn_ctx_switch)
	".p2align 4\n"
	KN_SYM(kn_ctx_switch) ":\n"
	"	pushq %rbp\n"
	"	pushq %rbx\n"
	"	pushq %r12\n"
	"	pushq %r13\n"
	"	pushq %r14\n"
	"	pushq %r15\n"
	"	subq $8, %rsp\n"
	"	stmxcsr (%rsp)\n"
	"	fnstcw 4(%rsp)\n"
	"	movq %rsp, (%rdi)\n"
	"	movq %rsi, %rsp\n"
	"	ldmxcsr (%rsp)\n"
	"	fldcw 4(%rsp)\n"
	"	addq $8, %rsp\n"
	"	popq %r15\n"
	"	popq %r14\n"
	"	popq %r13\n"
	"	popq %r12\n"
	"	popq %rbx\n"
	"	popq %rbp\n"
	"	ret\n"
	".globl " KN_SYM(kn_ctx_trampoline) "\n"
	KN_FN_TYPE(kn_ctx_trampoline)
	".p2align 4\n"
	KN_SYM(kn_ctx_trampoline) ":\n"
	"	movq %r12, %rdi\n"
#if defined(__APPLE__)
	"	call _kn_task_start\n"
#else
	"	call kn_task_start@PLT\n"
#endif
	"	ud2\n");

static void *ctx_init(char *top, KnTask *t) {
	uintptr_t sp = ((uintptr_t)top & ~(uintptr_t)15) - 80;
	uint64_t *s = (uint64_t *)sp;
	memset(s, 0, 80);
	((uint32_t *)s)[0] = 0x1F80; /* MXCSR default */
	((uint32_t *)s)[1] = 0x037F; /* x87 control word default */
	s[4] = (uint64_t)(uintptr_t)t; /* r12 */
	s[7] = (uint64_t)(uintptr_t)kn_ctx_trampoline;
	return (void *)sp;
}
#elif defined(__aarch64__)
/* Saves x19-x30 and d8-d15, stores sp in *save, and resumes at `to`. */
__asm__(
	".text\n"
	".globl " KN_SYM(kn_ctx_switch) "\n"
	KN_FN_TYPE(kn_ctx_switch)
	".p2align 4\n"
	KN_SYM(kn_ctx_switch) ":\n"
	"	sub sp, sp, #176\n"
	"	stp x19, x20, [sp, #0]\n"
	"	stp x21, x22, [sp, #16]\n"
	"	stp x23, x24, [sp, #32]\n"
	"	stp x25, x26, [sp, #48]\n"
	"	stp x27, x28, [sp, #64]\n"
	"	stp x29, x30, [sp, #80]\n"
	"	stp d8, d9, [sp, #96]\n"
	"	stp d10, d11, [sp, #112]\n"
	"	stp d12, d13, [sp, #128]\n"
	"	stp d14, d15, [sp, #144]\n"
	"	mov x2, sp\n"
	"	str x2, [x0]\n"
	"	mov sp, x1\n"
	"	ldp x19, x20, [sp, #0]\n"
	"	ldp x21, x22, [sp, #16]\n"
	"	ldp x23, x24, [sp, #32]\n"
	"	ldp x25, x26, [sp, #48]\n"
	"	ldp x27, x28, [sp, #64]\n"
	"	ldp x29, x30, [sp, #80]\n"
	"	ldp d8, d9, [sp, #96]\n"
	"	ldp d10, d11, [sp, #112]\n"
	"	ldp d12, d13, [sp, #128]\n"
	"	ldp d14, d15, [sp, #144]\n"
	"	add sp, sp, #176\n"
	"	ret\n"
	".globl " KN_SYM(kn_ctx_trampoline) "\n"
	KN_FN_TYPE(kn_ctx_trampoline)
	".p2align 4\n"
	KN_SYM(kn_ctx_trampoline) ":\n"
	"	mov x0, x19\n"
	"	bl " KN_SYM(kn_task_start) "\n"
	"	brk #0\n");

static void *ctx_init(char *top, KnTask *t) {
	uintptr_t sp = ((uintptr_t)top & ~(uintptr_t)15) - 176;
	uint64_t *s = (uint64_t *)sp;
	memset(s, 0, 176);
	s[0] = (uint64_t)(uintptr_t)t;                   /* x19 */
	s[11] = (uint64_t)(uintptr_t)kn_ctx_trampoline; /* x30 */
	return (void *)sp;
}
#else
#error "the Kiln runtime supports x86-64 and AArch64"
#endif

/* Stacks are mmap'd (committed lazily) with a guard page, and reused. */
static kn_mutex stack_lock;
static char *stack_pool[64];
static int stack_pool_len;
static size_t page_size;

static char *stack_alloc(void) {
	mutex_lock(&stack_lock);
	if (stack_pool_len > 0) {
		char *s = stack_pool[--stack_pool_len];
		mutex_unlock(&stack_lock);
		return s;
	}
	mutex_unlock(&stack_lock);
#ifdef MAP_NORESERVE
	void *p = mmap(NULL, KN_STACK_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON | MAP_NORESERVE, -1, 0);
#else
	void *p = mmap(NULL, KN_STACK_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
#endif
	if (p == MAP_FAILED)
		return NULL;
	mprotect(p, page_size, PROT_NONE);
	return (char *)p;
}

static int task_init_stack(KnTask *t) {
	t->stack = stack_alloc();
	if (!t->stack)
		return 0;
	t->sp = ctx_init(t->stack + KN_STACK_SIZE, t);
	return 1;
}

static void task_free_stack(KnTask *t) {
	mutex_lock(&stack_lock);
	if (stack_pool_len < 64) {
		stack_pool[stack_pool_len++] = t->stack;
		mutex_unlock(&stack_lock);
		return;
	}
	mutex_unlock(&stack_lock);
	munmap(t->stack, KN_STACK_SIZE);
}

static void switch_to_task(KnWorker *w, KnTask *t) { kn_ctx_switch(&w->sched_sp, t->sp); }

static void switch_to_sched(KnTask *t) { kn_ctx_switch(&t->sp, current_worker()->sched_sp); }

/* A fault inside the current task's guard page is a stack overflow. The
 * handler runs on a per-thread alternate stack, since the task's own stack
 * is exhausted. Any other fault falls through to the default action. */
static void segv_handler(int sig, siginfo_t *info, void *uctx) {
	(void)uctx;
	KnWorker *w = tls_worker;
	KnTask *t = w ? w->cur : NULL;
	char *addr = (char *)info->si_addr;
	if (t && t->stack && addr >= t->stack - page_size && addr < t->stack + 2 * page_size) {
		ssize_t r = write(2, STACK_OVERFLOW_MSG, sizeof STACK_OVERFLOW_MSG - 1);
		(void)r;
		_exit(134);
	}
	signal(sig, SIG_DFL);
}

static void worker_enter(KnWorker *w) {
	(void)w;
	stack_t ss;
	ss.ss_size = 64 * 1024;
	ss.ss_sp = malloc(ss.ss_size);
	ss.ss_flags = 0;
	if (ss.ss_sp)
		sigaltstack(&ss, NULL);
	static int installed;
	if (!installed) {
		installed = 1;
		struct sigaction sa;
		memset(&sa, 0, sizeof sa);
		sa.sa_sigaction = segv_handler;
		sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
		sigemptyset(&sa.sa_mask);
		sigaction(SIGSEGV, &sa, NULL);
		sigaction(SIGBUS, &sa, NULL);
	}
}

#endif

/* ----- run queues -------------------------------------------------------- */

static void q_push(KnWorker *w, KnTask *t) {
	t->next = NULL;
	mutex_lock(&w->lock);
	if (w->tail)
		w->tail->next = t;
	else
		w->head = t;
	w->tail = t;
	at_add(&w->count, 1);
	mutex_unlock(&w->lock);
}

static KnTask *q_pop(KnWorker *w) {
	if (at_load(&w->count) == 0)
		return NULL;
	mutex_lock(&w->lock);
	KnTask *t = w->head;
	if (t) {
		w->head = t->next;
		if (!w->head)
			w->tail = NULL;
		at_add(&w->count, -1);
	}
	mutex_unlock(&w->lock);
	return t;
}

static void g_push(KnTask *t) {
	t->next = NULL;
	mutex_lock(&global_lock);
	if (gtail)
		gtail->next = t;
	else
		ghead = t;
	gtail = t;
	at_add(&gcount, 1);
	mutex_unlock(&global_lock);
}

static KnTask *g_pop(void) {
	if (at_load(&gcount) == 0)
		return NULL;
	mutex_lock(&global_lock);
	KnTask *t = ghead;
	if (t) {
		ghead = t->next;
		if (!ghead)
			gtail = NULL;
		at_add(&gcount, -1);
	}
	mutex_unlock(&global_lock);
	return t;
}

/* Takes half of another worker's queue (at least one task). */
static KnTask *steal(KnWorker *self) {
	int64_t n = at_load(&nstarted);
	if (n < 2)
		return NULL;
	int64_t start = (int64_t)(next_rand() % (uint64_t)n);
	for (int64_t k = 0; k < n; k++) {
		KnWorker *v = &workers[(start + k) % n];
		if (v == self || at_load(&v->count) == 0)
			continue;
		mutex_lock(&v->lock);
		int64_t take = (v->count + 1) / 2;
		KnTask *first = NULL, *last = NULL;
		for (int64_t i = 0; i < take && v->head; i++) {
			KnTask *t = v->head;
			v->head = t->next;
			if (!v->head)
				v->tail = NULL;
			at_add(&v->count, -1);
			t->next = NULL;
			if (last)
				last->next = t;
			else
				first = t;
			last = t;
		}
		mutex_unlock(&v->lock);
		if (first) {
			KnTask *rest = first->next;
			while (rest) {
				KnTask *nx = rest->next;
				q_push(self, rest);
				rest = nx;
			}
			return first;
		}
	}
	return NULL;
}

static int any_work(void) {
	if (at_load(&gcount) > 0)
		return 1;
	int64_t n = at_load(&nstarted);
	for (int64_t i = 0; i < n; i++)
		if (at_load(&workers[i].count) > 0)
			return 1;
	return 0;
}

static KN_NORETURN void deadlock(void) {
	fflush(stdout);
	fputs("fatal error: all tasks are asleep - deadlock!\n", stderr);
	fflush(stderr);
	exit(2);
}

static KnTask *find_work(KnWorker *w) {
	for (;;) {
		KnTask *t = q_pop(w);
		if (!t)
			t = g_pop();
		if (!t)
			t = steal(w);
		if (t)
			return t;
		mutex_lock(&sched_lock);
		/* Counted as idle before re-checking, so a concurrent kn_ready either
		 * is seen here or sees us idle and signals. */
		int64_t now_idle = at_add(&idle, 1);
		if (any_work()) {
			at_add(&idle, -1);
			mutex_unlock(&sched_lock);
			continue;
		}
		if (now_idle == at_load(&nstarted) && at_load(&external) == 0)
			deadlock();
		cond_wait(&sched_cond, &sched_lock);
		at_add(&idle, -1);
		mutex_unlock(&sched_lock);
	}
}

/* Makes a parked or new task runnable. */
static void kn_ready(KnTask *t) {
	KnWorker *w = current_worker();
	if (w)
		q_push(w, t);
	else
		g_push(t);
	if (at_load(&idle) > 0) {
		mutex_lock(&sched_lock);
		cond_signal(&sched_cond);
		mutex_unlock(&sched_lock);
	}
}

/* Blocks the current task. `after(arg)` runs on the scheduler once the
 * task's context is saved (typically: release the lock of the queue the task
 * just joined). Someone must later call kn_ready on the task. */
static void kn_park(void (*after)(void *), void *arg) {
	KnWorker *w = current_worker();
	KnTask *t = w->cur;
	w->after = after;
	w->after_arg = arg;
	switch_to_sched(t);
}

int64_t kn_time_mono(void);

/* ----- internal API for the standard library (kiln_std.c) ---------------- */

void *kn_sched_current(void) { return current_task(); }
void kn_sched_park(void (*after)(void *), void *arg) { kn_park(after, arg); }
void kn_sched_ready(void *t) { kn_ready((KnTask *)t); }
void kn_sched_wait_begin(void) { at_add(&external, 1); }
void kn_sched_wait_end(void) { at_add(&external, -1); }

static void requeue(void *t) { g_push((KnTask *)t); }

/* Lets other tasks run: the current task goes to the back of the global queue. */
void kn_yield(void) {
	KnTask *t = current_task();
	if (!t)
		return;
	kn_park(requeue, t);
}

/* Called from loop back-edges every so often: yields if the current task
 * has run for more than 10ms and other tasks are waiting. */
void kn_maybe_yield(void) {
	KnWorker *w = current_worker();
	if (!w || !w->cur)
		return;
	if (kn_time_mono() - w->slice_start < 10000000)
		return;
	if (!any_work()) {
		w->slice_start = kn_time_mono();
		return;
	}
	kn_yield();
}

static void worker_loop(KnWorker *w) {
	for (;;) {
		KnTask *t = find_work(w);
		w->cur = t;
		w->slice_start = kn_time_mono();
		switch_to_task(w, t);
		w->cur = NULL;
		if (w->after) {
			void (*f)(void *) = w->after;
			void *a = w->after_arg;
			w->after = NULL;
			f(a);
		}
	}
}

#ifdef _WIN32
static DWORD WINAPI worker_main(LPVOID arg) {
#else
static void *worker_main(void *arg) {
#endif
	KnWorker *w = (KnWorker *)arg;
	tls_worker = w;
	worker_enter(w);
	worker_loop(w);
	return 0;
}

/* Starts the other workers on the first spawn. */
static void ensure_workers(void) {
	if (started_all || !at_cas32(&started_all, 0, 1))
		return;
	for (int64_t i = 1; i < nworkers; i++) {
		at_add(&nstarted, 1);
#ifdef _WIN32
		HANDLE h = CreateThread(NULL, 0, worker_main, &workers[i], 0, NULL);
		if (h)
			CloseHandle(h);
#else
		pthread_t th;
		if (pthread_create(&th, NULL, worker_main, &workers[i]) == 0)
			pthread_detach(th);
#endif
	}
}

static void task_free(void *p) {
	KnTask *t = (KnTask *)p;
	task_free_stack(t);
	free(t);
}

static KnTask *task_new(KnCode code, void *env, int free_env) {
	KnTask *t = (KnTask *)calloc(1, sizeof(KnTask));
	if (!t || !task_init_stack(t))
		kn_panic_str("out of memory (could not allocate a task stack)");
	t->code = code;
	t->env = env;
	t->free_env = free_env;
	t->ret = t->scratch;
	return t;
}

static void shared_complete(KnShared *sh);
static void scope_done(KnScope *s);

/* First code run on a new task's stack. */
void kn_task_start(KnTask *t) {
	t->code(t->ret, t->env);
	if (t->free_env)
		kn_free_env(t->env);
	if (t->shared)
		shared_complete(t->shared);
	if (t->scope)
		scope_done(t->scope);
	KnWorker *w = current_worker();
	w->after = task_free;
	w->after_arg = t;
	switch_to_sched(t);
	abort(); /* unreachable */
}

/* ----- Task<T> ----------------------------------------------------------- */

struct KnShared {
	kn_mutex lock;
	int32_t done, joined;
	KnTask *waiter;
	volatile int64_t refs; /* the handle and the running task */
	size_t size;
	void (*drop)(void *);
	char *result;
};

static void shared_release(KnShared *sh) {
	if (at_add(&sh->refs, -1) == 0) {
		if (sh->done && !sh->joined && sh->drop)
			sh->drop(sh->result);
		free(sh);
	}
}

static void shared_complete(KnShared *sh) {
	mutex_lock(&sh->lock);
	sh->done = 1;
	KnTask *w = sh->waiter;
	sh->waiter = NULL;
	mutex_unlock(&sh->lock);
	if (w)
		kn_ready(w);
	shared_release(sh);
}

/* `go`: starts a task running `code(result, env)`. `env` (env_size bytes,
 * from kn_alloc) is freed when the task finishes. Returns the Task handle. */
void *kn_go(KnCode code, void *env, size_t env_size, size_t ret_size, void (*drop)(void *)) {
	size_t head = (sizeof(KnShared) + 15) & ~(size_t)15;
	KnShared *sh = (KnShared *)calloc(1, head + ret_size + 1);
	if (!sh)
		kn_panic_str("out of memory");
	mutex_init(&sh->lock);
	sh->refs = 2;
	sh->size = ret_size;
	sh->drop = drop;
	sh->result = (char *)sh + head;
	KnTask *t = task_new(code, env, env_size != 0);
	t->ret = sh->result;
	t->shared = sh;
	ensure_workers();
	kn_ready(t);
	return sh;
}

/* `task.join()`: waits for the task and moves its result to `out`. */
void kn_task_join(void *h, void *out) {
	KnShared *sh = (KnShared *)h;
	mutex_lock(&sh->lock);
	if (!sh->done) {
		sh->waiter = current_task();
		kn_park(unlock_cb, &sh->lock);
	} else {
		mutex_unlock(&sh->lock);
	}
	memcpy(out, sh->result, sh->size);
	sh->joined = 1;
	shared_release(sh);
}

/* Dropping a Task handle detaches the task. */
void kn_task_drop(void *h) { shared_release((KnShared *)h); }

/* ----- scoped tasks ------------------------------------------------------ */

struct KnScope {
	kn_mutex lock;
	int64_t running;
	KnTask *waiter;
};

void *kn_scope_new(void) {
	KnScope *s = (KnScope *)calloc(1, sizeof(KnScope));
	if (!s)
		kn_panic_str("out of memory");
	mutex_init(&s->lock);
	return s;
}

/* `s.spawn(f)`: runs the closure `f` (code, env) as a task of scope `s`. */
void kn_scope_spawn(void *sp, KnCode code, void *env) {
	KnScope *s = (KnScope *)sp;
	mutex_lock(&s->lock);
	s->running++;
	mutex_unlock(&s->lock);
	KnTask *t = task_new(code, env, 0);
	t->scope = s;
	ensure_workers();
	kn_ready(t);
}

static void scope_done(KnScope *s) {
	mutex_lock(&s->lock);
	KnTask *w = NULL;
	if (--s->running == 0) {
		w = s->waiter;
		s->waiter = NULL;
	}
	mutex_unlock(&s->lock);
	if (w)
		kn_ready(w);
}

/* End of `scope(...)`: waits for the scope's tasks, then frees it. */
void kn_scope_wait(void *sp) {
	KnScope *s = (KnScope *)sp;
	mutex_lock(&s->lock);
	if (s->running > 0) {
		s->waiter = current_task();
		kn_park(unlock_cb, &s->lock);
	} else {
		mutex_unlock(&s->lock);
	}
	free(s);
}

/* ----- channels ---------------------------------------------------------- */

/* A task blocked on a channel. `sel_done` is shared by the waiters of one
 * `select`: whoever completes one of them first claims it. */
typedef struct KnWaiter {
	KnTask *task;
	void *elem;
	int64_t index;
	volatile int32_t *sel_done;
	int32_t ok;
	int32_t queued;
	struct KnWaiter *next, *prev;
} KnWaiter;

typedef struct {
	KnWaiter *head, *tail;
} KnWaitQ;

static void wq_push(KnWaitQ *q, KnWaiter *w) {
	w->next = NULL;
	w->prev = q->tail;
	if (q->tail)
		q->tail->next = w;
	else
		q->head = w;
	q->tail = w;
	w->queued = 1;
}

static void wq_remove(KnWaitQ *q, KnWaiter *w) {
	if (!w->queued)
		return;
	if (w->prev)
		w->prev->next = w->next;
	else
		q->head = w->next;
	if (w->next)
		w->next->prev = w->prev;
	else
		q->tail = w->prev;
	w->queued = 0;
}

/* The first waiter that can still be completed (skipping finished selects). */
static KnWaiter *wq_take(KnWaitQ *q) {
	while (q->head) {
		KnWaiter *w = q->head;
		wq_remove(q, w);
		if (!w->sel_done || at_cas32(w->sel_done, 0, 1))
			return w;
	}
	return NULL;
}

static void wake(KnWaiter *w, int32_t ok) {
	w->ok = ok;
	w->task->sel = w->index;
	kn_ready(w->task);
}

typedef struct KnChan {
	kn_mutex lock;
	size_t elem, cap, count, head;
	char *buf;
	int32_t closed;
	KnWaitQ sendq, recvq;
	int64_t senders, receivers;
	void (*drop)(void *);
} KnChan;

static char *slot(KnChan *c, size_t i) { return c->buf + ((c->head + i) % c->cap) * c->elem; }

/* `chan::<T>(cap)`: one sender and one receiver. */
void *kn_chan_new(size_t elem, size_t cap, void (*drop)(void *)) {
	KnChan *c = (KnChan *)calloc(1, sizeof(KnChan));
	if (!c)
		kn_panic_str("out of memory");
	mutex_init(&c->lock);
	c->elem = elem;
	c->cap = cap;
	c->drop = drop;
	c->senders = c->receivers = 1;
	if (cap) {
		c->buf = (char *)malloc(cap * (elem ? elem : 1));
		if (!c->buf)
			kn_panic_str("out of memory");
	}
	return c;
}

/* With the lock held: sends `src` if a receiver or buffer space is ready. */
static int try_send_locked(KnChan *c, void *src) {
	KnWaiter *r = wq_take(&c->recvq);
	if (r) {
		memcpy(r->elem, src, c->elem);
		wake(r, 1);
		return 1;
	}
	if (c->count < c->cap) {
		memcpy(slot(c, c->count), src, c->elem);
		c->count++;
		return 1;
	}
	return 0;
}

/* With the lock held: 1 received into `dst`, 0 would block, -1 closed and empty. */
static int try_recv_locked(KnChan *c, void *dst) {
	if (c->count > 0) {
		memcpy(dst, slot(c, 0), c->elem);
		c->head = (c->head + 1) % c->cap;
		c->count--;
		KnWaiter *s = wq_take(&c->sendq);
		if (s) {
			memcpy(slot(c, c->count), s->elem, c->elem);
			c->count++;
			wake(s, 1);
		}
		return 1;
	}
	KnWaiter *s = wq_take(&c->sendq);
	if (s) {
		memcpy(dst, s->elem, c->elem);
		wake(s, 1);
		return 1;
	}
	return c->closed ? -1 : 0;
}

static void close_locked(KnChan *c) {
	c->closed = 1;
	KnWaiter *w;
	while ((w = wq_take(&c->recvq)))
		wake(w, 0);
	while ((w = wq_take(&c->sendq)))
		wake(w, 0);
}

/* `tx <- v`: moves the value at `src` into the channel, blocking until a
 * receiver or buffer space is available. Panics if the channel is closed. */
void kn_chan_send(void *ch, void *src) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	if (c->closed) {
		mutex_unlock(&c->lock);
		kn_panic_str("send on a closed channel");
	}
	if (try_send_locked(c, src)) {
		mutex_unlock(&c->lock);
		return;
	}
	KnWaiter w;
	memset(&w, 0, sizeof w);
	w.task = current_task();
	w.elem = src;
	wq_push(&c->sendq, &w);
	kn_park(unlock_cb, &c->lock);
	if (!w.ok)
		kn_panic_str("send on a closed channel");
}

/* Receives into `dst`, blocking; returns 0 if the channel is closed and empty. */
int32_t kn_chan_recv(void *ch, void *dst) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	int r = try_recv_locked(c, dst);
	if (r != 0) {
		mutex_unlock(&c->lock);
		return r > 0;
	}
	KnWaiter w;
	memset(&w, 0, sizeof w);
	w.task = current_task();
	w.elem = dst;
	wq_push(&c->recvq, &w);
	kn_park(unlock_cb, &c->lock);
	return w.ok;
}

/* `tx.try_send(v)`: 0 sent, 1 full (or no receiver waiting), 2 closed. */
int32_t kn_chan_try_send(void *ch, void *src) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	int32_t r = c->closed ? 2 : try_send_locked(c, src) ? 0 : 1;
	mutex_unlock(&c->lock);
	return r;
}

/* `rx.try_recv()`: 0 received, 1 empty, 2 closed. */
int32_t kn_chan_try_recv(void *ch, void *dst) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	int r = try_recv_locked(c, dst);
	mutex_unlock(&c->lock);
	return r > 0 ? 0 : r == 0 ? 1 : 2;
}

/* `tx.close()`. Closing again has no effect. */
void kn_chan_close(void *ch) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	if (!c->closed)
		close_locked(c);
	mutex_unlock(&c->lock);
}

int64_t kn_chan_len(void *ch) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	int64_t n = (int64_t)c->count;
	mutex_unlock(&c->lock);
	return n;
}

int64_t kn_chan_cap(void *ch) { return (int64_t)((KnChan *)ch)->cap; }

static void chan_free(KnChan *c) {
	if (c->drop)
		for (size_t i = 0; i < c->count; i++)
			c->drop(slot(c, i));
	free(c->buf);
	free(c);
}

void kn_chan_clone_end(void *ch, int32_t sender) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	if (sender)
		c->senders++;
	else
		c->receivers++;
	mutex_unlock(&c->lock);
}

/* Dropping the last sender closes the channel; the last end frees it. */
void kn_chan_drop_end(void *ch, int32_t sender) {
	KnChan *c = (KnChan *)ch;
	mutex_lock(&c->lock);
	if (sender) {
		if (--c->senders == 0 && !c->closed)
			close_locked(c);
	} else {
		c->receivers--;
	}
	int gone = c->senders == 0 && c->receivers == 0;
	mutex_unlock(&c->lock);
	if (gone)
		chan_free(c);
}

/* ----- select ------------------------------------------------------------ */

typedef struct {
	void *chan;
	void *elem;    /* receive buffer, or the value to send */
	int64_t is_send;
} KnSelCase;

typedef struct {
	KnChan **chans;
	int64_t n;
} KnLockSet;

static void lock_all(KnLockSet *ls) {
	for (int64_t i = 0; i < ls->n; i++)
		mutex_lock(&ls->chans[i]->lock);
}

static void unlock_all(void *p) {
	KnLockSet *ls = (KnLockSet *)p;
	for (int64_t i = ls->n; i-- > 0;)
		mutex_unlock(&ls->chans[i]->lock);
}

static int chan_cmp(const void *a, const void *b) {
	uintptr_t x = (uintptr_t) * (KnChan *const *)a, y = (uintptr_t) * (KnChan *const *)b;
	return x < y ? -1 : x > y;
}

static void select_park_forever(void *unused) { (void)unused; }

/* `select`: performs one ready case, chosen at random, and returns its index;
 * returns n (the `_` arm) when none is ready and `has_default`; otherwise
 * blocks. Receives on closed channels are skipped. */
int64_t kn_select(KnSelCase *cs, int64_t n, int64_t has_default) {
	if (n == 0) {
		if (has_default)
			return 0;
		kn_park(select_park_forever, NULL); /* `select {}` blocks forever */
	}
	KnChan *small[8];
	KnWaiter wsmall[8];
	KnChan **chans = n <= 8 ? small : (KnChan **)malloc((size_t)n * sizeof(KnChan *));
	KnWaiter *ws = n <= 8 ? wsmall : (KnWaiter *)malloc((size_t)n * sizeof(KnWaiter));
	if (!chans || !ws)
		kn_panic_str("out of memory");
	/* Locks are taken in address order, each channel once. */
	for (int64_t i = 0; i < n; i++)
		chans[i] = (KnChan *)cs[i].chan;
	qsort(chans, (size_t)n, sizeof(KnChan *), chan_cmp);
	int64_t u = 0;
	for (int64_t i = 0; i < n; i++)
		if (u == 0 || chans[u - 1] != chans[i])
			chans[u++] = chans[i];
	KnLockSet ls = {chans, u};
	int64_t result = -1;
	for (;;) {
		lock_all(&ls);
		int64_t start = (int64_t)(next_rand() % (uint64_t)n);
		int all_closed = 1;
		for (int64_t k = 0; k < n && result < 0; k++) {
			int64_t i = (start + k) % n;
			KnChan *c = (KnChan *)cs[i].chan;
			if (cs[i].is_send) {
				if (c->closed) {
					unlock_all(&ls);
					kn_panic_str("send on a closed channel");
				}
				all_closed = 0;
				if (try_send_locked(c, cs[i].elem))
					result = i;
			} else {
				int r = try_recv_locked(c, cs[i].elem);
				if (r > 0)
					result = i;
				else if (r == 0)
					all_closed = 0;
			}
		}
		if (result >= 0 || has_default) {
			unlock_all(&ls);
			if (result < 0)
				result = n;
			break;
		}
		if (all_closed) {
			unlock_all(&ls);
			kn_panic_str("select: all channels are closed and there is no `_` arm");
		}
		/* Wait on every case; the first to complete claims the select. */
		volatile int32_t done = 0;
		KnTask *self = current_task();
		self->sel = -1;
		for (int64_t i = 0; i < n; i++) {
			KnChan *c = (KnChan *)cs[i].chan;
			memset(&ws[i], 0, sizeof(KnWaiter));
			ws[i].task = self;
			ws[i].elem = cs[i].elem;
			ws[i].index = i;
			ws[i].sel_done = &done;
			wq_push(cs[i].is_send ? &c->sendq : &c->recvq, &ws[i]);
		}
		kn_park(unlock_all, &ls);
		lock_all(&ls);
		for (int64_t i = 0; i < n; i++) {
			KnChan *c = (KnChan *)cs[i].chan;
			wq_remove(cs[i].is_send ? &c->sendq : &c->recvq, &ws[i]);
		}
		unlock_all(&ls);
		int64_t win = self->sel;
		if (win >= 0 && ws[win].ok) {
			result = win;
			break;
		}
		if (win >= 0 && cs[win].is_send)
			kn_panic_str("send on a closed channel");
		/* A receive channel was closed: look again. */
	}
	if (chans != small)
		free(chans);
	if (ws != wsmall)
		free(ws);
	return result;
}

/* ----- program entry ----------------------------------------------------- */

int32_t kn_main(int32_t argc, int64_t argv);

static int g_argc;
static char **g_argv;

static void main_task(void *ret, void *env) {
	(void)ret;
	(void)env;
	int32_t code = kn_main(g_argc, (int64_t)(intptr_t)g_argv);
	fflush(stdout);
	fflush(stderr);
	exit(code);
}

static int64_t cpu_count(void) {
#ifdef _WIN32
	SYSTEM_INFO si;
	GetSystemInfo(&si);
	return (int64_t)si.dwNumberOfProcessors;
#else
	long n = sysconf(_SC_NPROCESSORS_ONLN);
	return n > 0 ? (int64_t)n : 1;
#endif
}

/* The program's entry point: runs `main` as the first task, with the main
 * thread as worker 0. The program exits when `main` returns. */
void kn_os_set_args(int argc, char **argv);

int main(int argc, char **argv) {
	g_argc = argc;
	g_argv = argv;
	kn_os_set_args(argc, argv);
	nworkers = cpu_count();
	const char *env = getenv("KILN_WORKERS");
	if (env && atoi(env) > 0)
		nworkers = atoi(env);
	if (nworkers > 256)
		nworkers = 256;
	workers = (KnWorker *)calloc((size_t)nworkers, sizeof(KnWorker));
	if (!workers)
		return 1;
	for (int64_t i = 0; i < nworkers; i++) {
		mutex_init(&workers[i].lock);
		workers[i].rng = 0x9E3779B97F4A7C15ull * (uint64_t)(i + 1);
	}
	mutex_init(&global_lock);
	mutex_init(&sched_lock);
	cond_init(&sched_cond);
#ifndef _WIN32
	mutex_init(&stack_lock);
	page_size = (size_t)sysconf(_SC_PAGESIZE);
#endif
	tls_worker = &workers[0];
	worker_enter(&workers[0]);
	g_push(task_new(main_task, NULL, 0));
	worker_loop(&workers[0]);
	return 0;
}
