/*
 * Runtime support for the Kiln standard library: clocks and timers, task-aware
 * locks, atomics, the network poller and sockets, files, and the OS.
 *
 * The library itself is written in Kiln (crates/kiln_resolve/std); it calls
 * the `kn_*` functions here for anything that needs the OS or the scheduler.
 * Conventions:
 *   - Strings and byte buffers arrive as (pointer, length); output strings are
 *     appended to a Kiln `String` (a KnVec) with kn_string_push.
 *   - Fallible calls return a non-negative result or a negative error code:
 *     -((kind << 32) | os_code), where `kind` is an io::ErrorKind number
 *     (see KN_E_* below, mirrored in std/io.kn) and os_code is errno, a
 *     Windows error, or a getaddrinfo code (for KN_E_HOST_NOT_FOUND).
 *   - Blocking network calls park the calling task, not the thread: sockets
 *     are non-blocking and a poller thread (epoll, kqueue, or WSAPoll) wakes
 *     the task when the socket is ready. Sleeping parks the task on a timer
 *     thread. File and process I/O block the worker thread (like Go's
 *     syscalls, but without handing the worker's queue off).
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <direct.h>
#include <fcntl.h>
#include <io.h>
#include <process.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <shellapi.h>
#pragma comment(lib, "ws2_32.lib")
#pragma comment(lib, "shell32.lib")
#else
#include <arpa/inet.h>
#include <dirent.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <crt_externs.h>
#include <mach-o/dyld.h>
#include <sys/event.h>
#else
#include <sys/epoll.h>
#endif
extern char **environ;
#endif

#ifdef _MSC_VER
#define KN_NORETURN __declspec(noreturn)
#else
#define KN_NORETURN __attribute__((noreturn))
#endif

typedef struct {
	uint8_t *ptr;
	size_t cap;
	size_t len;
} KnVec;

KN_NORETURN void kn_panic_str(const char *msg);
void kn_string_push(KnVec *s, const uint8_t *p, size_t len);
void *kn_sched_current(void);
void kn_sched_park(void (*after)(void *), void *arg);
void kn_sched_ready(void *t);
void kn_sched_wait_begin(void);
void kn_sched_wait_end(void);
void kn_yield(void);

static void push_cstr(KnVec *s, const char *c) { kn_string_push(s, (const uint8_t *)c, strlen(c)); }

/* A NUL-terminated copy of (p, n); free with free(). */
static char *cstr(const uint8_t *p, size_t n) {
	char *c = (char *)malloc(n + 1);
	if (!c)
		kn_panic_str("out of memory");
	memcpy(c, p, n);
	c[n] = 0;
	return c;
}

/* ----- platform locks ------------------------------------------------------ */

#ifdef _WIN32
typedef SRWLOCK lock_t;
typedef CONDITION_VARIABLE cond_t;
static void lk_init(lock_t *m) { InitializeSRWLock(m); }
static void lk(lock_t *m) { AcquireSRWLockExclusive(m); }
static void ulk(lock_t *m) { ReleaseSRWLockExclusive(m); }
static void cv_wait(cond_t *c, lock_t *m) { SleepConditionVariableSRW(c, m, INFINITE, 0); }
static void cv_wait_ns(cond_t *c, lock_t *m, int64_t ns) {
	DWORD ms = (DWORD)((ns + 999999) / 1000000);
	SleepConditionVariableSRW(c, m, ms ? ms : 1, 0);
}
static void cv_signal(cond_t *c) { WakeConditionVariable(c); }
#define LOCK_INIT SRWLOCK_INIT
#define COND_INIT CONDITION_VARIABLE_INIT
#else
typedef pthread_mutex_t lock_t;
typedef pthread_cond_t cond_t;
static void lk_init(lock_t *m) { pthread_mutex_init(m, NULL); }
static void lk(lock_t *m) { pthread_mutex_lock(m); }
static void ulk(lock_t *m) { pthread_mutex_unlock(m); }
static void cv_wait(cond_t *c, lock_t *m) { pthread_cond_wait(c, m); }
static void cv_wait_ns(cond_t *c, lock_t *m, int64_t ns) {
#if defined(__APPLE__)
	struct timespec ts = {(time_t)(ns / 1000000000), (long)(ns % 1000000000)};
	pthread_cond_timedwait_relative_np(c, m, &ts);
#else
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	int64_t t = (int64_t)ts.tv_nsec + ns;
	ts.tv_sec += (time_t)(t / 1000000000);
	ts.tv_nsec = (long)(t % 1000000000);
	pthread_cond_timedwait(c, m, &ts);
#endif
}
static void cv_signal(cond_t *c) { pthread_cond_signal(c); }
#define LOCK_INIT PTHREAD_MUTEX_INITIALIZER
#define COND_INIT PTHREAD_COND_INITIALIZER
#endif

static void unlock_cb(void *m) { ulk((lock_t *)m); }

static int start_thread(void (*f)(void)) {
#ifdef _WIN32
	HANDLE h = CreateThread(NULL, 0, (LPTHREAD_START_ROUTINE)(void *)f, NULL, 0, NULL);
	if (!h)
		return -1;
	CloseHandle(h);
	return 0;
#else
	pthread_t th;
	if (pthread_create(&th, NULL, (void *(*)(void *))(void *)f, NULL) != 0)
		return -1;
	pthread_detach(th);
	return 0;
#endif
}

/* ----- errors -------------------------------------------------------------- */

enum {
	KN_E_OTHER = 0,
	KN_E_NOT_FOUND = 1,
	KN_E_PERMISSION_DENIED = 2,
	KN_E_CONNECTION_REFUSED = 3,
	KN_E_CONNECTION_RESET = 4,
	KN_E_CONNECTION_ABORTED = 5,
	KN_E_NOT_CONNECTED = 6,
	KN_E_ADDR_IN_USE = 7,
	KN_E_ADDR_NOT_AVAILABLE = 8,
	KN_E_BROKEN_PIPE = 9,
	KN_E_ALREADY_EXISTS = 10,
	KN_E_WOULD_BLOCK = 11,
	KN_E_INVALID_INPUT = 12,
	KN_E_INVALID_DATA = 13,
	KN_E_TIMED_OUT = 14,
	KN_E_WRITE_ZERO = 15,
	KN_E_INTERRUPTED = 16,
	KN_E_UNEXPECTED_EOF = 17,
	KN_E_IS_A_DIRECTORY = 18,
	KN_E_NOT_A_DIRECTORY = 19,
	KN_E_DIRECTORY_NOT_EMPTY = 20,
	KN_E_UNSUPPORTED = 21,
	KN_E_HOST_NOT_FOUND = 22,
};

static int64_t mkerr(int64_t kind, int64_t code) { return -((kind << 32) | (code & 0xffffffff)); }

static int64_t kind_of_errno(int e) {
	switch (e) {
	case ENOENT: return KN_E_NOT_FOUND;
	case EACCES:
	case EPERM: return KN_E_PERMISSION_DENIED;
	case ECONNREFUSED: return KN_E_CONNECTION_REFUSED;
	case ECONNRESET: return KN_E_CONNECTION_RESET;
	case ECONNABORTED: return KN_E_CONNECTION_ABORTED;
	case ENOTCONN: return KN_E_NOT_CONNECTED;
	case EADDRINUSE: return KN_E_ADDR_IN_USE;
	case EADDRNOTAVAIL: return KN_E_ADDR_NOT_AVAILABLE;
	case EPIPE: return KN_E_BROKEN_PIPE;
	case EEXIST: return KN_E_ALREADY_EXISTS;
	case EAGAIN: return KN_E_WOULD_BLOCK;
	case EINVAL: return KN_E_INVALID_INPUT;
	case ETIMEDOUT: return KN_E_TIMED_OUT;
	case EINTR: return KN_E_INTERRUPTED;
	case EISDIR: return KN_E_IS_A_DIRECTORY;
	case ENOTDIR: return KN_E_NOT_A_DIRECTORY;
	case ENOTEMPTY: return KN_E_DIRECTORY_NOT_EMPTY;
	default: return KN_E_OTHER;
	}
}

static int64_t errno_err(void) { return mkerr(kind_of_errno(errno), errno); }

#ifdef _WIN32
static int64_t kind_of_win(DWORD e) {
	switch (e) {
	case ERROR_FILE_NOT_FOUND:
	case ERROR_PATH_NOT_FOUND: return KN_E_NOT_FOUND;
	case ERROR_ACCESS_DENIED: return KN_E_PERMISSION_DENIED;
	case ERROR_ALREADY_EXISTS:
	case ERROR_FILE_EXISTS: return KN_E_ALREADY_EXISTS;
	case ERROR_DIR_NOT_EMPTY: return KN_E_DIRECTORY_NOT_EMPTY;
	case ERROR_DIRECTORY: return KN_E_NOT_A_DIRECTORY;
	case ERROR_BROKEN_PIPE: return KN_E_BROKEN_PIPE;
	case WSAECONNREFUSED: return KN_E_CONNECTION_REFUSED;
	case WSAECONNRESET: return KN_E_CONNECTION_RESET;
	case WSAECONNABORTED: return KN_E_CONNECTION_ABORTED;
	case WSAENOTCONN: return KN_E_NOT_CONNECTED;
	case WSAEADDRINUSE: return KN_E_ADDR_IN_USE;
	case WSAEADDRNOTAVAIL: return KN_E_ADDR_NOT_AVAILABLE;
	case WSAEWOULDBLOCK: return KN_E_WOULD_BLOCK;
	case WSAEINVAL: return KN_E_INVALID_INPUT;
	case WSAETIMEDOUT: return KN_E_TIMED_OUT;
	case WSAEACCES: return KN_E_PERMISSION_DENIED;
	default: return KN_E_OTHER;
	}
}
/* Windows error codes are tagged with bit 30 so the message lookup knows. */
static int64_t win_err(DWORD e) { return mkerr(kind_of_win(e), (int64_t)e | (1 << 30)); }
static int64_t sock_err(void) { return win_err((DWORD)WSAGetLastError()); }
#else
static int64_t sock_err(void) { return errno_err(); }
#endif

/* Appends the OS's message for error `code` (the low 32 bits of an error). */
void kn_os_error_message(int64_t kind, int64_t code, KnVec *out) {
	if (kind == KN_E_HOST_NOT_FOUND) {
#ifdef _WIN32
		push_cstr(out, gai_strerrorA((int)code));
#else
		push_cstr(out, gai_strerror((int)code));
#endif
		return;
	}
#ifdef _WIN32
	if (code & (1 << 30)) {
		char buf[512];
		DWORD n = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL,
					 (DWORD)(code & ~(1 << 30)), 0, buf, sizeof buf, NULL);
		while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r' || buf[n - 1] == '.'))
			n--;
		kn_string_push(out, (const uint8_t *)buf, n);
		return;
	}
#endif
	push_cstr(out, strerror((int)code));
}

/* ----- clocks -------------------------------------------------------------- */

/* Monotonic nanoseconds (for Instant). */
int64_t kn_time_mono(void) {
#ifdef _WIN32
	static LARGE_INTEGER freq;
	LARGE_INTEGER c;
	if (!freq.QuadPart)
		QueryPerformanceFrequency(&freq);
	QueryPerformanceCounter(&c);
	return (int64_t)((double)c.QuadPart * 1e9 / (double)freq.QuadPart);
#else
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
#endif
}

/* Wall-clock nanoseconds since the Unix epoch (for SystemTime). */
int64_t kn_time_unix(void) {
#ifdef _WIN32
	FILETIME ft;
	GetSystemTimePreciseAsFileTime(&ft);
	uint64_t t = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime;
	return (int64_t)((t - 116444736000000000ull) * 100);
#else
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
#endif
}

/* ----- timers: sleeping parks the task ----------------------------------- */

typedef struct {
	int64_t when;
	void *task;
} TimerEnt;

static lock_t tlock = LOCK_INIT;
static cond_t tcond = COND_INIT;
static TimerEnt *theap;
static size_t tlen, tcap;
static int tstarted;

static void heap_push(TimerEnt e) {
	if (tlen == tcap) {
		tcap = tcap ? tcap * 2 : 64;
		theap = (TimerEnt *)realloc(theap, tcap * sizeof(TimerEnt));
		if (!theap)
			kn_panic_str("out of memory");
	}
	size_t i = tlen++;
	while (i > 0 && theap[(i - 1) / 2].when > e.when) {
		theap[i] = theap[(i - 1) / 2];
		i = (i - 1) / 2;
	}
	theap[i] = e;
}

static TimerEnt heap_pop(void) {
	TimerEnt top = theap[0], last = theap[--tlen];
	size_t i = 0;
	for (;;) {
		size_t c = 2 * i + 1;
		if (c >= tlen)
			break;
		if (c + 1 < tlen && theap[c + 1].when < theap[c].when)
			c++;
		if (theap[c].when >= last.when)
			break;
		theap[i] = theap[c];
		i = c;
	}
	if (tlen)
		theap[i] = last;
	return top;
}

static void timer_thread(void) {
	lk(&tlock);
	for (;;) {
		if (tlen == 0) {
			cv_wait(&tcond, &tlock);
			continue;
		}
		int64_t now = kn_time_mono();
		if (theap[0].when <= now) {
			TimerEnt e = heap_pop();
			kn_sched_ready(e.task);
			kn_sched_wait_end();
			continue;
		}
		cv_wait_ns(&tcond, &tlock, theap[0].when - now);
	}
}

/* Parks the current task for `ns` nanoseconds (just yields if ns <= 0). */
void kn_time_sleep(int64_t ns) {
	if (ns <= 0) {
		kn_yield();
		return;
	}
	TimerEnt e = {kn_time_mono() + ns, kn_sched_current()};
	lk(&tlock);
	if (!tstarted) {
		tstarted = 1;
		if (start_thread(timer_thread) != 0)
			kn_panic_str("cannot start the timer thread");
	}
	heap_push(e);
	cv_signal(&tcond);
	kn_sched_wait_begin();
	kn_sched_park(unlock_cb, &tlock);
}

/* `std::runtime::yield_now`. */
void kn_task_yield(void) { kn_yield(); }

/* ----- task-aware locks ----------------------------------------------------- */

typedef struct WaitNode {
	void *task;
	int write;
	struct WaitNode *next;
} WaitNode;

typedef struct {
	WaitNode *head, *tail;
} WaitQ;

static void q_add(WaitQ *q, WaitNode *n) {
	n->next = NULL;
	if (q->tail)
		q->tail->next = n;
	else
		q->head = n;
	q->tail = n;
}

static WaitNode *q_take(WaitQ *q) {
	WaitNode *n = q->head;
	if (n) {
		q->head = n->next;
		if (!q->head)
			q->tail = NULL;
	}
	return n;
}

/* Mutex: FIFO hand-off to waiting tasks. */
typedef struct {
	lock_t lock;
	int locked;
	WaitQ q;
} KnMutex;

int64_t kn_mutex_new(void) {
	KnMutex *m = (KnMutex *)calloc(1, sizeof(KnMutex));
	if (!m)
		kn_panic_str("out of memory");
	lk_init(&m->lock);
	return (int64_t)(intptr_t)m;
}

void kn_mutex_free(int64_t h) { free((void *)(intptr_t)h); }

void kn_mutex_lock(int64_t h) {
	KnMutex *m = (KnMutex *)(intptr_t)h;
	lk(&m->lock);
	if (!m->locked) {
		m->locked = 1;
		ulk(&m->lock);
		return;
	}
	WaitNode n = {kn_sched_current(), 0, NULL};
	q_add(&m->q, &n);
	kn_sched_park(unlock_cb, &m->lock); /* ownership is handed to us */
}

int8_t kn_mutex_try_lock(int64_t h) {
	KnMutex *m = (KnMutex *)(intptr_t)h;
	lk(&m->lock);
	int ok = !m->locked;
	if (ok)
		m->locked = 1;
	ulk(&m->lock);
	return (int8_t)ok;
}

void kn_mutex_unlock(int64_t h) {
	KnMutex *m = (KnMutex *)(intptr_t)h;
	lk(&m->lock);
	WaitNode *n = q_take(&m->q);
	if (!n)
		m->locked = 0;
	ulk(&m->lock);
	if (n)
		kn_sched_ready(n->task);
}

/* RwLock: many readers or one writer; FIFO, so a waiting writer blocks new readers. */
typedef struct {
	lock_t lock;
	int64_t readers;
	int writer;
	WaitQ q;
} KnRwLock;

int64_t kn_rwlock_new(void) {
	KnRwLock *l = (KnRwLock *)calloc(1, sizeof(KnRwLock));
	if (!l)
		kn_panic_str("out of memory");
	lk_init(&l->lock);
	return (int64_t)(intptr_t)l;
}

void kn_rwlock_free(int64_t h) { free((void *)(intptr_t)h); }

/* Wakes the waiters at the front that can now proceed (lock held). */
static void rw_grant(KnRwLock *l) {
	while (l->q.head) {
		WaitNode *n = l->q.head;
		if (n->write) {
			if (l->readers == 0 && !l->writer) {
				l->writer = 1;
				q_take(&l->q);
				kn_sched_ready(n->task);
			}
			return;
		}
		if (l->writer)
			return;
		l->readers++;
		q_take(&l->q);
		kn_sched_ready(n->task);
	}
}

void kn_rwlock_read(int64_t h) {
	KnRwLock *l = (KnRwLock *)(intptr_t)h;
	lk(&l->lock);
	if (!l->writer && !l->q.head) {
		l->readers++;
		ulk(&l->lock);
		return;
	}
	WaitNode n = {kn_sched_current(), 0, NULL};
	q_add(&l->q, &n);
	kn_sched_park(unlock_cb, &l->lock);
}

void kn_rwlock_write(int64_t h) {
	KnRwLock *l = (KnRwLock *)(intptr_t)h;
	lk(&l->lock);
	if (!l->writer && l->readers == 0 && !l->q.head) {
		l->writer = 1;
		ulk(&l->lock);
		return;
	}
	WaitNode n = {kn_sched_current(), 1, NULL};
	q_add(&l->q, &n);
	kn_sched_park(unlock_cb, &l->lock);
}

void kn_rwlock_read_unlock(int64_t h) {
	KnRwLock *l = (KnRwLock *)(intptr_t)h;
	lk(&l->lock);
	l->readers--;
	rw_grant(l);
	ulk(&l->lock);
}

void kn_rwlock_write_unlock(int64_t h) {
	KnRwLock *l = (KnRwLock *)(intptr_t)h;
	lk(&l->lock);
	l->writer = 0;
	rw_grant(l);
	ulk(&l->lock);
}

/* WaitGroup. */
typedef struct {
	lock_t lock;
	int64_t count;
	WaitQ q;
} KnWaitGroup;

int64_t kn_wg_new(void) {
	KnWaitGroup *g = (KnWaitGroup *)calloc(1, sizeof(KnWaitGroup));
	if (!g)
		kn_panic_str("out of memory");
	lk_init(&g->lock);
	return (int64_t)(intptr_t)g;
}

void kn_wg_free(int64_t h) { free((void *)(intptr_t)h); }

void kn_wg_add(int64_t h, int64_t n) {
	KnWaitGroup *g = (KnWaitGroup *)(intptr_t)h;
	lk(&g->lock);
	g->count += n;
	if (g->count < 0) {
		ulk(&g->lock);
		kn_panic_str("sync: negative WaitGroup counter");
	}
	WaitNode *w = NULL;
	if (g->count == 0) {
		w = g->q.head;
		g->q.head = g->q.tail = NULL;
	}
	ulk(&g->lock);
	while (w) {
		WaitNode *next = w->next; /* read before waking: the node lives on the waiter's stack */
		kn_sched_ready(w->task);
		w = next;
	}
}

void kn_wg_wait(int64_t h) {
	KnWaitGroup *g = (KnWaitGroup *)(intptr_t)h;
	lk(&g->lock);
	if (g->count == 0) {
		ulk(&g->lock);
		return;
	}
	WaitNode n = {kn_sched_current(), 0, NULL};
	q_add(&g->q, &n);
	kn_sched_park(unlock_cb, &g->lock);
}

/* ----- atomics (sequentially consistent) ----------------------------------- */

#ifdef _MSC_VER
int64_t kn_atomic_load(int64_t *p) { return InterlockedCompareExchange64((volatile LONG64 *)p, 0, 0); }
void kn_atomic_store(int64_t *p, int64_t v) { InterlockedExchange64((volatile LONG64 *)p, v); }
int64_t kn_atomic_add(int64_t *p, int64_t v) { return InterlockedExchangeAdd64((volatile LONG64 *)p, v) + v; }
int64_t kn_atomic_swap(int64_t *p, int64_t v) { return InterlockedExchange64((volatile LONG64 *)p, v); }
int8_t kn_atomic_cas(int64_t *p, int64_t old, int64_t nw) {
	return InterlockedCompareExchange64((volatile LONG64 *)p, nw, old) == old;
}
#else
int64_t kn_atomic_load(int64_t *p) { return __atomic_load_n(p, __ATOMIC_SEQ_CST); }
void kn_atomic_store(int64_t *p, int64_t v) { __atomic_store_n(p, v, __ATOMIC_SEQ_CST); }
int64_t kn_atomic_add(int64_t *p, int64_t v) { return __atomic_add_fetch(p, v, __ATOMIC_SEQ_CST); }
int64_t kn_atomic_swap(int64_t *p, int64_t v) { return __atomic_exchange_n(p, v, __ATOMIC_SEQ_CST); }
int8_t kn_atomic_cas(int64_t *p, int64_t old, int64_t nw) {
	return (int8_t)__atomic_compare_exchange_n(p, &old, nw, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
#endif

/* ----- network poller ------------------------------------------------------ */

#ifdef _WIN32
typedef SOCKET sock_t;
#define BAD_SOCK INVALID_SOCKET
#define close_sock closesocket
#else
typedef int sock_t;
#define BAD_SOCK (-1)
#define close_sock close
#endif

/* A socket handle as seen by Kiln. */
typedef struct KnSock {
	sock_t fd;
	lock_t lock;
	void *rtask, *wtask; /* tasks waiting to read / write */
	int registered;      /* epoll: added to the set */
	int armed;           /* WSAPoll: in the armed list */
} KnSock;

static lock_t plock = LOCK_INIT;
static int pstarted;

#if defined(_WIN32)
static KnSock **armed;
static size_t narmed, carmed;
static SOCKET wake_rx = INVALID_SOCKET, wake_tx = INVALID_SOCKET;
static struct sockaddr_in wake_addr;
#elif defined(__APPLE__)
static int pfd = -1;
#else
static int pfd = -1;
#endif

/* Takes the waiting tasks for the ready directions (lock held). */
static int take_ready(KnSock *s, int rd, int wr, void **out) {
	int n = 0;
	if (rd && s->rtask) {
		out[n++] = s->rtask;
		s->rtask = NULL;
	}
	if (wr && s->wtask) {
		out[n++] = s->wtask;
		s->wtask = NULL;
	}
	return n;
}

static void wake_all(void **ts, int n) {
	for (int i = 0; i < n; i++) {
		kn_sched_ready(ts[i]);
		kn_sched_wait_end();
	}
}

#if defined(_WIN32)
static void poller_thread(void) {
	WSAPOLLFD *fds = NULL;
	KnSock **socks = NULL;
	size_t cap = 0;
	for (;;) {
		lk(&plock);
		size_t n = narmed + 1;
		if (n > cap) {
			cap = n * 2;
			fds = (WSAPOLLFD *)realloc(fds, cap * sizeof(WSAPOLLFD));
			socks = (KnSock **)realloc(socks, cap * sizeof(KnSock *));
		}
		fds[0].fd = wake_rx;
		fds[0].events = POLLRDNORM;
		fds[0].revents = 0;
		for (size_t i = 0; i < narmed; i++) {
			KnSock *s = armed[i];
			socks[i + 1] = s;
			fds[i + 1].fd = s->fd;
			fds[i + 1].events = (short)((s->rtask ? POLLRDNORM : 0) | (s->wtask ? POLLWRNORM : 0));
			fds[i + 1].revents = 0;
		}
		ulk(&plock);
		int r = WSAPoll(fds, (ULONG)n, -1);
		if (r <= 0)
			continue;
		if (fds[0].revents) {
			char buf[64];
			while (recv(wake_rx, buf, sizeof buf, 0) > 0) {
			}
		}
		for (size_t i = 1; i < n; i++) {
			short ev = fds[i].revents;
			if (!ev)
				continue;
			KnSock *s = socks[i];
			int bad = ev & (POLLERR | POLLHUP | POLLNVAL);
			void *ts[2];
			lk(&plock);
			lk(&s->lock);
			int k = take_ready(s, bad || (ev & POLLRDNORM), bad || (ev & POLLWRNORM), ts);
			if (!s->rtask && !s->wtask && s->armed) {
				s->armed = 0;
				for (size_t j = 0; j < narmed; j++)
					if (armed[j] == s) {
						armed[j] = armed[--narmed];
						break;
					}
			}
			ulk(&s->lock);
			ulk(&plock);
			wake_all(ts, k);
		}
	}
}

static void poller_kick(void) { sendto(wake_tx, "x", 1, 0, (struct sockaddr *)&wake_addr, sizeof wake_addr); }

static void poller_init(void) {
	wake_rx = socket(AF_INET, SOCK_DGRAM, 0);
	wake_tx = socket(AF_INET, SOCK_DGRAM, 0);
	memset(&wake_addr, 0, sizeof wake_addr);
	wake_addr.sin_family = AF_INET;
	wake_addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	bind(wake_rx, (struct sockaddr *)&wake_addr, sizeof wake_addr);
	int len = sizeof wake_addr;
	getsockname(wake_rx, (struct sockaddr *)&wake_addr, &len);
	u_long nb = 1;
	ioctlsocket(wake_rx, FIONBIO, &nb);
}
#elif defined(__APPLE__)
static void poller_thread(void) {
	struct kevent evs[64];
	for (;;) {
		int n = kevent(pfd, NULL, 0, evs, 64, NULL);
		for (int i = 0; i < n; i++) {
			KnSock *s = (KnSock *)evs[i].udata;
			void *ts[2];
			lk(&s->lock);
			int k = take_ready(s, evs[i].filter == EVFILT_READ, evs[i].filter == EVFILT_WRITE, ts);
			ulk(&s->lock);
			wake_all(ts, k);
		}
	}
}

static void poller_init(void) { pfd = kqueue(); }
#else
static void poller_thread(void) {
	struct epoll_event evs[64];
	for (;;) {
		int n = epoll_wait(pfd, evs, 64, -1);
		for (int i = 0; i < n; i++) {
			KnSock *s = (KnSock *)evs[i].data.ptr;
			uint32_t e = evs[i].events;
			int bad = e & (EPOLLERR | EPOLLHUP);
			void *ts[2];
			lk(&s->lock);
			int k = take_ready(s, bad || (e & (EPOLLIN | EPOLLRDHUP)), bad || (e & EPOLLOUT), ts);
			if (s->rtask || s->wtask) {
				/* The other direction is still waiting: re-arm it. */
				struct epoll_event ev;
				ev.events = (s->rtask ? EPOLLIN | EPOLLRDHUP : 0) | (s->wtask ? EPOLLOUT : 0) | EPOLLONESHOT;
				ev.data.ptr = s;
				epoll_ctl(pfd, EPOLL_CTL_MOD, s->fd, &ev);
			}
			ulk(&s->lock);
			wake_all(ts, k);
		}
	}
}

static void poller_init(void) { pfd = epoll_create1(EPOLL_CLOEXEC); }
#endif

static void poller_start(void) {
	lk(&plock);
	if (!pstarted) {
		pstarted = 1;
		poller_init();
		if (start_thread(poller_thread) != 0)
			kn_panic_str("cannot start the network poller");
	}
	ulk(&plock);
}

typedef struct {
	KnSock *s;
	int write;
	void *task;
} ArmReq;

/* Runs on the scheduler after the waiting task is parked. */
static void arm(void *p) {
	ArmReq *r = (ArmReq *)p;
	KnSock *s = r->s;
	lk(&s->lock);
	if (r->write)
		s->wtask = r->task;
	else
		s->rtask = r->task;
#if defined(_WIN32)
	ulk(&s->lock);
	lk(&plock);
	lk(&s->lock);
	if (!s->armed && (s->rtask || s->wtask)) {
		if (narmed == carmed) {
			carmed = carmed ? carmed * 2 : 64;
			armed = (KnSock **)realloc(armed, carmed * sizeof(KnSock *));
		}
		armed[narmed++] = s;
		s->armed = 1;
	}
	ulk(&s->lock);
	ulk(&plock);
	poller_kick();
#elif defined(__APPLE__)
	struct kevent ev;
	EV_SET(&ev, s->fd, r->write ? EVFILT_WRITE : EVFILT_READ, EV_ADD | EV_ENABLE | EV_ONESHOT, 0, 0, s);
	kevent(pfd, &ev, 1, NULL, 0, NULL);
	ulk(&s->lock);
#else
	struct epoll_event ev;
	ev.events = (s->rtask ? EPOLLIN | EPOLLRDHUP : 0) | (s->wtask ? EPOLLOUT : 0) | EPOLLONESHOT;
	ev.data.ptr = s;
	if (s->registered) {
		epoll_ctl(pfd, EPOLL_CTL_MOD, s->fd, &ev);
	} else {
		s->registered = 1;
		epoll_ctl(pfd, EPOLL_CTL_ADD, s->fd, &ev);
	}
	ulk(&s->lock);
#endif
}

/* Parks the current task until `s` is readable (write = 0) or writable. */
static void wait_io(KnSock *s, int write) {
	poller_start();
	ArmReq r = {s, write, kn_sched_current()};
	kn_sched_wait_begin();
	kn_sched_park(arm, &r);
}

static int would_block(void) {
#ifdef _WIN32
	int e = WSAGetLastError();
	return e == WSAEWOULDBLOCK || e == WSAEINPROGRESS;
#else
	return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINPROGRESS;
#endif
}

/* ----- sockets --------------------------------------------------------------- */

static void net_init(void) {
#ifdef _WIN32
	static volatile LONG done;
	if (InterlockedCompareExchange(&done, 1, 0) == 0) {
		WSADATA d;
		WSAStartup(MAKEWORD(2, 2), &d);
	}
#endif
}

static int set_nonblocking(sock_t fd) {
#ifdef _WIN32
	u_long nb = 1;
	return ioctlsocket(fd, FIONBIO, &nb) == 0 ? 0 : -1;
#else
	int fl = fcntl(fd, F_GETFL, 0);
	if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) < 0)
		return -1;
	fcntl(fd, F_SETFD, FD_CLOEXEC);
#ifdef __APPLE__
	int one = 1;
	setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#endif
	return 0;
#endif
}

static KnSock *sock_wrap(sock_t fd) {
	KnSock *s = (KnSock *)calloc(1, sizeof(KnSock));
	if (!s)
		kn_panic_str("out of memory");
	s->fd = fd;
	lk_init(&s->lock);
	return s;
}

/* Resolves host (may be empty: any address) and port. */
static int64_t resolve(const uint8_t *host, size_t hl, int64_t port, int socktype, int passive, struct addrinfo **out) {
	net_init();
	char *h = hl ? cstr(host, hl) : NULL;
	/* Strip [brackets] from IPv6 literals. */
	if (h && h[0] == '[' && hl > 2 && h[hl - 1] == ']') {
		memmove(h, h + 1, hl - 2);
		h[hl - 2] = 0;
	}
	char ps[16];
	snprintf(ps, sizeof ps, "%d", (int)port);
	struct addrinfo hints;
	memset(&hints, 0, sizeof hints);
	hints.ai_family = h ? AF_UNSPEC : AF_INET;
	hints.ai_socktype = socktype;
	hints.ai_flags = passive ? AI_PASSIVE : 0;
	int r = getaddrinfo(h, ps, &hints, out);
	free(h);
	if (r != 0)
		return mkerr(KN_E_HOST_NOT_FOUND, r);
	return 0;
}

/* `TcpListener::bind`: returns a socket handle or an error. */
int64_t kn_net_listen(const uint8_t *host, size_t hl, int64_t port) {
	struct addrinfo *ai, *p;
	int64_t err = resolve(host, hl, port, SOCK_STREAM, 1, &ai);
	if (err)
		return err;
	err = mkerr(KN_E_ADDR_NOT_AVAILABLE, 0);
	for (p = ai; p; p = p->ai_next) {
		sock_t fd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
		if (fd == BAD_SOCK) {
			err = sock_err();
			continue;
		}
#ifndef _WIN32
		int one = 1;
		setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
#endif
		if (bind(fd, p->ai_addr, (int)p->ai_addrlen) != 0 || listen(fd, 1024) != 0 || set_nonblocking(fd) != 0) {
			err = sock_err();
			close_sock(fd);
			continue;
		}
		freeaddrinfo(ai);
		return (int64_t)(intptr_t)sock_wrap(fd);
	}
	freeaddrinfo(ai);
	return err;
}

/* Accepts a connection, parking until one arrives. */
int64_t kn_net_accept(int64_t h) {
	KnSock *s = (KnSock *)(intptr_t)h;
	for (;;) {
		sock_t fd = accept(s->fd, NULL, NULL);
		if (fd != BAD_SOCK) {
			if (set_nonblocking(fd) != 0) {
				int64_t e = sock_err();
				close_sock(fd);
				return e;
			}
			int one = 1;
			setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
			return (int64_t)(intptr_t)sock_wrap(fd);
		}
		if (!would_block()) {
#ifndef _WIN32
			if (errno == EINTR || errno == ECONNABORTED)
				continue;
#endif
			return sock_err();
		}
		wait_io(s, 0);
	}
}

/* `TcpStream::connect`: tries each resolved address. */
int64_t kn_net_connect(const uint8_t *host, size_t hl, int64_t port) {
	struct addrinfo *ai, *p;
	int64_t err = resolve(host, hl, port, SOCK_STREAM, 0, &ai);
	if (err)
		return err;
	err = mkerr(KN_E_CONNECTION_REFUSED, 0);
	for (p = ai; p; p = p->ai_next) {
		sock_t fd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
		if (fd == BAD_SOCK) {
			err = sock_err();
			continue;
		}
		if (set_nonblocking(fd) != 0) {
			err = sock_err();
			close_sock(fd);
			continue;
		}
		KnSock *s = sock_wrap(fd);
		int r = connect(fd, p->ai_addr, (int)p->ai_addrlen);
		if (r != 0 && would_block()) {
			wait_io(s, 1);
			int soerr = 0;
#ifdef _WIN32
			int len = sizeof soerr;
#else
			socklen_t len = sizeof soerr;
#endif
			getsockopt(fd, SOL_SOCKET, SO_ERROR, (char *)&soerr, &len);
			if (soerr == 0)
				r = 0;
			else {
#ifdef _WIN32
				err = win_err((DWORD)soerr);
#else
				err = mkerr(kind_of_errno(soerr), soerr);
#endif
			}
		} else if (r != 0) {
			err = sock_err();
		}
		if (r == 0) {
			int one = 1;
			setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
			freeaddrinfo(ai);
			return (int64_t)(intptr_t)s;
		}
		close_sock(fd);
		free(s);
	}
	freeaddrinfo(ai);
	return err;
}

/* Reads up to n bytes, parking until data arrives. 0 means end of stream. */
int64_t kn_net_read(int64_t h, uint8_t *buf, size_t n) {
	KnSock *s = (KnSock *)(intptr_t)h;
	if (n > 0x40000000)
		n = 0x40000000;
	for (;;) {
		int64_t r = recv(s->fd, (char *)buf, (int)n, 0);
		if (r >= 0)
			return r;
		if (!would_block()) {
#ifndef _WIN32
			if (errno == EINTR)
				continue;
#endif
			return sock_err();
		}
		wait_io(s, 0);
	}
}

/* Writes up to n bytes, parking while the socket buffer is full. */
int64_t kn_net_write(int64_t h, const uint8_t *buf, size_t n) {
	KnSock *s = (KnSock *)(intptr_t)h;
	if (n > 0x40000000)
		n = 0x40000000;
	for (;;) {
#if defined(_WIN32) || defined(__APPLE__)
		int64_t r = send(s->fd, (const char *)buf, (int)n, 0);
#else
		int64_t r = send(s->fd, buf, n, MSG_NOSIGNAL);
#endif
		if (r >= 0)
			return r;
		if (!would_block()) {
#ifndef _WIN32
			if (errno == EINTR)
				continue;
#endif
			return sock_err();
		}
		wait_io(s, 1);
	}
}

/* how: 0 = read, 1 = write, 2 = both. */
int64_t kn_net_shutdown(int64_t h, int64_t how) {
	KnSock *s = (KnSock *)(intptr_t)h;
#ifdef _WIN32
	int m = how == 0 ? SD_RECEIVE : how == 1 ? SD_SEND : SD_BOTH;
#else
	int m = how == 0 ? SHUT_RD : how == 1 ? SHUT_WR : SHUT_RDWR;
#endif
	return shutdown(s->fd, m) == 0 ? 0 : sock_err();
}

void kn_net_close(int64_t h) {
	KnSock *s = (KnSock *)(intptr_t)h;
#if !defined(_WIN32) && !defined(__APPLE__)
	if (s->registered)
		epoll_ctl(pfd, EPOLL_CTL_DEL, s->fd, NULL);
#endif
	close_sock(s->fd);
	free(s);
}

static void fmt_addr(const struct sockaddr *sa, KnVec *out) {
	char ip[INET6_ADDRSTRLEN + 1] = {0};
	char buf[INET6_ADDRSTRLEN + 16];
	if (sa->sa_family == AF_INET6) {
		const struct sockaddr_in6 *a = (const struct sockaddr_in6 *)sa;
		inet_ntop(AF_INET6, (void *)&a->sin6_addr, ip, sizeof ip);
		snprintf(buf, sizeof buf, "[%s]:%d", ip, ntohs(a->sin6_port));
	} else {
		const struct sockaddr_in *a = (const struct sockaddr_in *)sa;
		inet_ntop(AF_INET, (void *)&a->sin_addr, ip, sizeof ip);
		snprintf(buf, sizeof buf, "%s:%d", ip, ntohs(a->sin_port));
	}
	push_cstr(out, buf);
}

/* Appends "ip:port" for the local (peer = 0) or remote end. */
int64_t kn_net_addr(int64_t h, int64_t peer, KnVec *out) {
	KnSock *s = (KnSock *)(intptr_t)h;
	struct sockaddr_storage ss;
#ifdef _WIN32
	int len = sizeof ss;
#else
	socklen_t len = sizeof ss;
#endif
	int r = peer ? getpeername(s->fd, (struct sockaddr *)&ss, &len) : getsockname(s->fd, (struct sockaddr *)&ss, &len);
	if (r != 0)
		return sock_err();
	fmt_addr((struct sockaddr *)&ss, out);
	return 0;
}

/* `UdpSocket::bind`. */
int64_t kn_net_udp_bind(const uint8_t *host, size_t hl, int64_t port) {
	struct addrinfo *ai, *p;
	int64_t err = resolve(host, hl, port, SOCK_DGRAM, 1, &ai);
	if (err)
		return err;
	err = mkerr(KN_E_ADDR_NOT_AVAILABLE, 0);
	for (p = ai; p; p = p->ai_next) {
		sock_t fd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
		if (fd == BAD_SOCK) {
			err = sock_err();
			continue;
		}
		if (bind(fd, p->ai_addr, (int)p->ai_addrlen) != 0 || set_nonblocking(fd) != 0) {
			err = sock_err();
			close_sock(fd);
			continue;
		}
		freeaddrinfo(ai);
		return (int64_t)(intptr_t)sock_wrap(fd);
	}
	freeaddrinfo(ai);
	return err;
}

int64_t kn_net_send_to(int64_t h, const uint8_t *buf, size_t n, const uint8_t *host, size_t hl, int64_t port) {
	KnSock *s = (KnSock *)(intptr_t)h;
	struct addrinfo *ai;
	int64_t err = resolve(host, hl, port, SOCK_DGRAM, 0, &ai);
	if (err)
		return err;
	for (;;) {
		int64_t r = sendto(s->fd, (const char *)buf, (int)n, 0, ai->ai_addr, (int)ai->ai_addrlen);
		if (r >= 0) {
			freeaddrinfo(ai);
			return r;
		}
		if (!would_block()) {
			err = sock_err();
			freeaddrinfo(ai);
			return err;
		}
		wait_io(s, 1);
	}
}

/* Receives one datagram; appends the sender's address to `from`. */
int64_t kn_net_recv_from(int64_t h, uint8_t *buf, size_t n, KnVec *from) {
	KnSock *s = (KnSock *)(intptr_t)h;
	for (;;) {
		struct sockaddr_storage ss;
#ifdef _WIN32
		int len = sizeof ss;
#else
		socklen_t len = sizeof ss;
#endif
		int64_t r = recvfrom(s->fd, (char *)buf, (int)n, 0, (struct sockaddr *)&ss, &len);
		if (r >= 0) {
			fmt_addr((struct sockaddr *)&ss, from);
			return r;
		}
		if (!would_block())
			return sock_err();
		wait_io(s, 0);
	}
}

/* ----- files ------------------------------------------------------------------ */

#ifdef _WIN32
/* UTF-8 (p, n) to a NUL-terminated wide string; free with free(). */
static wchar_t *wide(const uint8_t *p, size_t n) {
	int m = MultiByteToWideChar(CP_UTF8, 0, (const char *)p, (int)n, NULL, 0);
	wchar_t *w = (wchar_t *)malloc(((size_t)m + 1) * sizeof(wchar_t));
	if (!w)
		kn_panic_str("out of memory");
	MultiByteToWideChar(CP_UTF8, 0, (const char *)p, (int)n, w, m);
	w[m] = 0;
	return w;
}

static void push_wide(KnVec *out, const wchar_t *w) {
	int m = WideCharToMultiByte(CP_UTF8, 0, w, -1, NULL, 0, NULL, NULL);
	if (m <= 1)
		return;
	char *b = (char *)malloc((size_t)m);
	WideCharToMultiByte(CP_UTF8, 0, w, -1, b, m, NULL, NULL);
	kn_string_push(out, (const uint8_t *)b, (size_t)m - 1);
	free(b);
}
#endif

/* Open flags (std/fs.kn): 1 read, 2 write, 4 append, 8 create, 16 truncate, 32 create_new. */
int64_t kn_fs_open(const uint8_t *path, size_t pl, int64_t flags) {
	int f = 0;
	int rd = flags & 1, wr = (flags & 2) || (flags & 4);
	f = rd && wr ? O_RDWR : wr ? O_WRONLY : O_RDONLY;
	if (flags & 4)
		f |= O_APPEND;
	if (flags & 8)
		f |= O_CREAT;
	if (flags & 16)
		f |= O_TRUNC;
	if (flags & 32)
		f |= O_CREAT | O_EXCL;
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	int fd = _wopen(w, f | O_BINARY | O_NOINHERIT, _S_IREAD | _S_IWRITE);
	free(w);
#else
	char *c = cstr(path, pl);
	int fd = open(c, f | O_CLOEXEC, 0666);
	free(c);
#endif
	if (fd < 0)
		return errno_err();
	return fd;
}

int64_t kn_fs_read(int64_t fd, uint8_t *buf, size_t n) {
	if (n > 0x40000000)
		n = 0x40000000;
	for (;;) {
#ifdef _WIN32
		int64_t r = _read((int)fd, buf, (unsigned)n);
#else
		int64_t r = read((int)fd, buf, n);
#endif
		if (r >= 0)
			return r;
		if (errno != EINTR)
			return errno_err();
	}
}

int64_t kn_fs_write(int64_t fd, const uint8_t *buf, size_t n) {
	if (n > 0x40000000)
		n = 0x40000000;
	for (;;) {
#ifdef _WIN32
		int64_t r = _write((int)fd, buf, (unsigned)n);
#else
		int64_t r = write((int)fd, buf, n);
#endif
		if (r >= 0)
			return r;
		if (errno != EINTR)
			return errno_err();
	}
}

/* whence: 0 start, 1 current, 2 end. Returns the new position. */
int64_t kn_fs_seek(int64_t fd, int64_t off, int64_t whence) {
#ifdef _WIN32
	int64_t r = _lseeki64((int)fd, off, (int)whence);
#else
	int64_t r = lseek((int)fd, off, (int)whence);
#endif
	return r < 0 ? errno_err() : r;
}

int64_t kn_fs_sync(int64_t fd) {
#ifdef _WIN32
	return _commit((int)fd) == 0 ? 0 : errno_err();
#else
	return fsync((int)fd) == 0 ? 0 : errno_err();
#endif
}

void kn_fs_close(int64_t fd) {
#ifdef _WIN32
	_close((int)fd);
#else
	close((int)fd);
#endif
}

/* Metadata: kind (1 file, 2 dir, 3 symlink, 0 other), size, mtime (ns since epoch). */
int64_t kn_fs_stat(const uint8_t *path, size_t pl, int64_t follow, int64_t *kind, int64_t *size, int64_t *mtime) {
#ifdef _WIN32
	(void)follow;
	wchar_t *w = wide(path, pl);
	struct _stat64 st;
	int r = _wstat64(w, &st);
	free(w);
	if (r != 0)
		return errno_err();
	*kind = (st.st_mode & _S_IFDIR) ? 2 : (st.st_mode & _S_IFREG) ? 1 : 0;
	*size = st.st_size;
	*mtime = (int64_t)st.st_mtime * 1000000000;
#else
	char *c = cstr(path, pl);
	struct stat st;
	int r = follow ? stat(c, &st) : lstat(c, &st);
	free(c);
	if (r != 0)
		return errno_err();
	*kind = S_ISREG(st.st_mode) ? 1 : S_ISDIR(st.st_mode) ? 2 : S_ISLNK(st.st_mode) ? 3 : 0;
	*size = st.st_size;
#if defined(__APPLE__)
	*mtime = (int64_t)st.st_mtimespec.tv_sec * 1000000000 + st.st_mtimespec.tv_nsec;
#else
	*mtime = (int64_t)st.st_mtim.tv_sec * 1000000000 + st.st_mtim.tv_nsec;
#endif
#endif
	return 0;
}

int64_t kn_fs_mkdir(const uint8_t *path, size_t pl) {
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	int r = _wmkdir(w);
	free(w);
#else
	char *c = cstr(path, pl);
	int r = mkdir(c, 0777);
	free(c);
#endif
	return r == 0 ? 0 : errno_err();
}

int64_t kn_fs_remove_file(const uint8_t *path, size_t pl) {
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	int r = _wremove(w);
	free(w);
#else
	char *c = cstr(path, pl);
	int r = unlink(c);
	free(c);
#endif
	return r == 0 ? 0 : errno_err();
}

int64_t kn_fs_remove_dir(const uint8_t *path, size_t pl) {
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	int r = _wrmdir(w);
	free(w);
#else
	char *c = cstr(path, pl);
	int r = rmdir(c);
	free(c);
#endif
	return r == 0 ? 0 : errno_err();
}

int64_t kn_fs_rename(const uint8_t *from, size_t fl, const uint8_t *to, size_t tl) {
#ifdef _WIN32
	wchar_t *a = wide(from, fl), *b = wide(to, tl);
	BOOL ok = MoveFileExW(a, b, MOVEFILE_REPLACE_EXISTING);
	free(a);
	free(b);
	return ok ? 0 : win_err(GetLastError());
#else
	char *a = cstr(from, fl), *b = cstr(to, tl);
	int r = rename(a, b);
	free(a);
	free(b);
	return r == 0 ? 0 : errno_err();
#endif
}

/* Appends the names in directory `path` (excluding . and ..), each followed by '\n'. */
int64_t kn_fs_read_dir(const uint8_t *path, size_t pl, KnVec *out) {
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	size_t wl = wcslen(w);
	wchar_t *pat = (wchar_t *)malloc((wl + 3) * sizeof(wchar_t));
	memcpy(pat, w, wl * sizeof(wchar_t));
	pat[wl] = L'\\';
	pat[wl + 1] = L'*';
	pat[wl + 2] = 0;
	free(w);
	WIN32_FIND_DATAW fd;
	HANDLE h = FindFirstFileW(pat, &fd);
	free(pat);
	if (h == INVALID_HANDLE_VALUE)
		return win_err(GetLastError());
	do {
		if (wcscmp(fd.cFileName, L".") && wcscmp(fd.cFileName, L"..")) {
			push_wide(out, fd.cFileName);
			push_cstr(out, "\n");
		}
	} while (FindNextFileW(h, &fd));
	FindClose(h);
	return 0;
#else
	char *c = cstr(path, pl);
	DIR *d = opendir(c);
	free(c);
	if (!d)
		return errno_err();
	struct dirent *e;
	while ((e = readdir(d))) {
		if (strcmp(e->d_name, ".") && strcmp(e->d_name, "..")) {
			push_cstr(out, e->d_name);
			push_cstr(out, "\n");
		}
	}
	closedir(d);
	return 0;
#endif
}

/* Appends the current directory. */
int64_t kn_fs_getcwd(KnVec *out) {
#ifdef _WIN32
	wchar_t buf[32768];
	if (!_wgetcwd(buf, 32768))
		return errno_err();
	push_wide(out, buf);
#else
	char buf[4096];
	if (!getcwd(buf, sizeof buf))
		return errno_err();
	push_cstr(out, buf);
#endif
	return 0;
}

int64_t kn_fs_chdir(const uint8_t *path, size_t pl) {
#ifdef _WIN32
	wchar_t *w = wide(path, pl);
	int r = _wchdir(w);
	free(w);
#else
	char *c = cstr(path, pl);
	int r = chdir(c);
	free(c);
#endif
	return r == 0 ? 0 : errno_err();
}

/* Appends the system's temporary directory. */
void kn_fs_temp_dir(KnVec *out) {
#ifdef _WIN32
	wchar_t buf[MAX_PATH + 2];
	DWORD n = GetTempPathW(MAX_PATH + 1, buf);
	if (n > 1 && (buf[n - 1] == L'\\' || buf[n - 1] == L'/'))
		buf[n - 1] = 0;
	push_wide(out, buf);
#else
	const char *t = getenv("TMPDIR");
	if (!t || !*t)
		t = "/tmp";
	size_t n = strlen(t);
	while (n > 1 && t[n - 1] == '/')
		n--;
	kn_string_push(out, (const uint8_t *)t, n);
#endif
}

/* ----- standard streams ---------------------------------------------------- */

/* fd: 0 stdin (read only), 1 stdout, 2 stderr. Output shares stdio's buffers
 * with print/println so the two interleave in order. */
int64_t kn_io_write(int64_t fd, const uint8_t *buf, size_t n) {
	FILE *f = fd == 2 ? stderr : stdout;
	size_t w = fwrite(buf, 1, n, f);
	if (w < n && ferror(f))
		return errno_err();
	return (int64_t)w;
}

int64_t kn_io_flush(int64_t fd) {
	FILE *f = fd == 2 ? stderr : stdout;
	return fflush(f) == 0 ? 0 : errno_err();
}

int64_t kn_io_read_stdin(uint8_t *buf, size_t n) {
	fflush(stdout);
	return kn_fs_read(0, buf, n);
}

/* ----- environment, process -------------------------------------------------- */

static int g_argc;
static char **g_argv;

/* Called by the entry point with the program's arguments. */
void kn_os_set_args(int argc, char **argv) {
	g_argc = argc;
	g_argv = argv;
#ifndef _WIN32
	signal(SIGPIPE, SIG_IGN);
#endif
}

int64_t kn_os_argc(void) {
#ifdef _WIN32
	int n = 0;
	LPWSTR *a = CommandLineToArgvW(GetCommandLineW(), &n);
	LocalFree(a);
	return n;
#else
	return g_argc;
#endif
}

void kn_os_arg(int64_t i, KnVec *out) {
#ifdef _WIN32
	int n = 0;
	LPWSTR *a = CommandLineToArgvW(GetCommandLineW(), &n);
	if (a && i < n)
		push_wide(out, a[i]);
	LocalFree(a);
#else
	if (i < g_argc)
		push_cstr(out, g_argv[i]);
#endif
}

/* Appends the value of variable `name`; returns 0 if it is not set. */
int8_t kn_os_getenv(const uint8_t *name, size_t nl, KnVec *out) {
#ifdef _WIN32
	wchar_t *w = wide(name, nl);
	DWORD n = GetEnvironmentVariableW(w, NULL, 0);
	if (n == 0) {
		free(w);
		return 0;
	}
	wchar_t *v = (wchar_t *)malloc(n * sizeof(wchar_t));
	GetEnvironmentVariableW(w, v, n);
	push_wide(out, v);
	free(v);
	free(w);
	return 1;
#else
	char *c = cstr(name, nl);
	const char *v = getenv(c);
	free(c);
	if (!v)
		return 0;
	push_cstr(out, v);
	return 1;
#endif
}

int64_t kn_os_setenv(const uint8_t *name, size_t nl, const uint8_t *val, size_t vl) {
#ifdef _WIN32
	wchar_t *a = wide(name, nl), *b = wide(val, vl);
	BOOL ok = SetEnvironmentVariableW(a, b);
	free(a);
	free(b);
	return ok ? 0 : win_err(GetLastError());
#else
	char *a = cstr(name, nl), *b = cstr(val, vl);
	int r = setenv(a, b, 1);
	free(a);
	free(b);
	return r == 0 ? 0 : errno_err();
#endif
}

int64_t kn_os_unsetenv(const uint8_t *name, size_t nl) {
#ifdef _WIN32
	wchar_t *a = wide(name, nl);
	SetEnvironmentVariableW(a, NULL);
	free(a);
	return 0;
#else
	char *a = cstr(name, nl);
	int r = unsetenv(a);
	free(a);
	return r == 0 ? 0 : errno_err();
#endif
}

/* Appends every "NAME=value", each followed by '\n'. */
void kn_os_environ(KnVec *out) {
#ifdef _WIN32
	LPWCH env = GetEnvironmentStringsW();
	for (LPWCH p = env; *p; p += wcslen(p) + 1) {
		if (*p == L'=')
			continue; /* per-drive cwd entries */
		push_wide(out, p);
		push_cstr(out, "\n");
	}
	FreeEnvironmentStringsW(env);
#else
	for (char **e = environ; *e; e++) {
		push_cstr(out, *e);
		push_cstr(out, "\n");
	}
#endif
}

KN_NORETURN void kn_os_exit(int64_t code) {
	fflush(stdout);
	fflush(stderr);
	exit((int)code);
}

int64_t kn_os_getpid(void) {
#ifdef _WIN32
	return (int64_t)GetCurrentProcessId();
#else
	return (int64_t)getpid();
#endif
}

int64_t kn_os_hostname(KnVec *out) {
	char buf[256];
	net_init();
	if (gethostname(buf, sizeof buf) != 0)
		return sock_err();
	buf[255] = 0;
	push_cstr(out, buf);
	return 0;
}

/* Appends the path of the running executable. */
int64_t kn_os_executable(KnVec *out) {
#ifdef _WIN32
	wchar_t buf[32768];
	DWORD n = GetModuleFileNameW(NULL, buf, 32768);
	if (n == 0)
		return win_err(GetLastError());
	push_wide(out, buf);
	return 0;
#elif defined(__APPLE__)
	char buf[4096];
	uint32_t n = sizeof buf;
	if (_NSGetExecutablePath(buf, &n) != 0)
		return mkerr(KN_E_OTHER, 0);
	push_cstr(out, buf);
	return 0;
#else
	char buf[4096];
	ssize_t n = readlink("/proc/self/exe", buf, sizeof buf - 1);
	if (n < 0)
		return errno_err();
	kn_string_push(out, (const uint8_t *)buf, (size_t)n);
	return 0;
#endif
}

#ifdef _WIN32
typedef struct {
	HANDLE h;
	KnVec *v;
} PipeRead;

static DWORD WINAPI pipe_reader(LPVOID p) {
	PipeRead *r = (PipeRead *)p;
	char buf[4096];
	DWORD n;
	while (ReadFile(r->h, buf, sizeof buf, &n, NULL) && n > 0)
		kn_string_push(r->v, (const uint8_t *)buf, n);
	return 0;
}
#endif

/*
 * Runs a program to completion. `argv` is the program and its arguments, each
 * followed by '\n' (so arguments cannot contain newlines). stdin_mode /
 * capture: 0 = inherit, 1 = capture into `out`/`err` (or null stdin). Returns
 * the exit status (128 + signal for a signal on Unix) or an error.
 */
int64_t kn_os_run(const uint8_t *argv, size_t al, int64_t capture, KnVec *out, KnVec *err) {
	/* Split the argument list. */
	size_t argc = 0;
	for (size_t i = 0; i < al; i++)
		argc += argv[i] == '\n';
	if (argc == 0)
		return mkerr(KN_E_INVALID_INPUT, 0);
	char **args = (char **)calloc(argc + 1, sizeof(char *));
	size_t k = 0, start = 0;
	for (size_t i = 0; i < al; i++)
		if (argv[i] == '\n') {
			args[k++] = cstr(argv + start, i - start);
			start = i + 1;
		}
	int64_t result;
	fflush(stdout);
	fflush(stderr);
#ifdef _WIN32
	/* Build a command line with Windows quoting rules. */
	KnVec cl = {0};
	for (size_t i = 0; i < argc; i++) {
		if (i)
			push_cstr(&cl, " ");
		const char *a = args[i];
		int quote = !*a || strpbrk(a, " \t\"") != NULL;
		if (quote)
			push_cstr(&cl, "\"");
		size_t bs = 0;
		for (const char *p = a;; p++) {
			if (*p == '\\') {
				bs++;
				continue;
			}
			if (*p == '"' || (*p == 0 && quote)) {
				for (size_t j = 0; j < bs * 2 + (*p == '"'); j++)
					push_cstr(&cl, "\\");
			} else {
				for (size_t j = 0; j < bs; j++)
					push_cstr(&cl, "\\");
			}
			bs = 0;
			if (!*p)
				break;
			kn_string_push(&cl, (const uint8_t *)p, 1);
		}
		if (quote)
			push_cstr(&cl, "\"");
	}
	wchar_t *wcl = wide(cl.ptr, cl.len);
	free(cl.ptr);
	SECURITY_ATTRIBUTES sa = {sizeof sa, NULL, TRUE};
	HANDLE or = NULL, ow = NULL, er = NULL, ew = NULL;
	STARTUPINFOW si;
	memset(&si, 0, sizeof si);
	si.cb = sizeof si;
	if (capture) {
		CreatePipe(&or, &ow, &sa, 0);
		CreatePipe(&er, &ew, &sa, 0);
		SetHandleInformation(or, HANDLE_FLAG_INHERIT, 0);
		SetHandleInformation(er, HANDLE_FLAG_INHERIT, 0);
		si.dwFlags = STARTF_USESTDHANDLES;
		si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
		si.hStdOutput = ow;
		si.hStdError = ew;
	}
	PROCESS_INFORMATION pi;
	BOOL ok = CreateProcessW(NULL, wcl, NULL, NULL, TRUE, 0, NULL, NULL, &si, &pi);
	free(wcl);
	if (!ok) {
		result = win_err(GetLastError());
		if (capture) {
			CloseHandle(or);
			CloseHandle(ow);
			CloseHandle(er);
			CloseHandle(ew);
		}
	} else {
		if (capture) {
			CloseHandle(ow);
			CloseHandle(ew);
			/* Read stderr on a helper thread so neither pipe can fill up and stall the child. */
			PipeRead pr = {er, err};
			HANDLE th = CreateThread(NULL, 0, pipe_reader, &pr, 0, NULL);
			char buf[4096];
			DWORD n;
			while (ReadFile(or, buf, sizeof buf, &n, NULL) && n > 0)
				kn_string_push(out, (const uint8_t *)buf, n);
			if (th) {
				WaitForSingleObject(th, INFINITE);
				CloseHandle(th);
			} else {
				pipe_reader(&pr);
			}
			CloseHandle(or);
			CloseHandle(er);
		}
		WaitForSingleObject(pi.hProcess, INFINITE);
		DWORD code = 0;
		GetExitCodeProcess(pi.hProcess, &code);
		CloseHandle(pi.hProcess);
		CloseHandle(pi.hThread);
		result = (int64_t)code;
	}
#else
	int po[2] = {-1, -1}, pe[2] = {-1, -1};
	if (capture && (pipe(po) != 0 || pipe(pe) != 0)) {
		result = errno_err();
		goto done;
	}
	int ep[2];
	if (pipe(ep) != 0) {
		result = errno_err();
		goto done;
	}
	fcntl(ep[1], F_SETFD, FD_CLOEXEC);
	pid_t pid = fork();
	if (pid < 0) {
		result = errno_err();
		close(ep[0]);
		close(ep[1]);
		goto done;
	}
	if (pid == 0) {
		close(ep[0]);
		if (capture) {
			dup2(po[1], 1);
			dup2(pe[1], 2);
			close(po[0]);
			close(po[1]);
			close(pe[0]);
			close(pe[1]);
		}
		signal(SIGPIPE, SIG_DFL);
		execvp(args[0], args);
		int e = errno;
		ssize_t w = write(ep[1], &e, sizeof e);
		(void)w;
		_exit(127);
	}
	close(ep[1]);
	int child_errno = 0;
	ssize_t got = read(ep[0], &child_errno, sizeof child_errno);
	close(ep[0]);
	if (capture) {
		close(po[1]);
		close(pe[1]);
		/* Drain both pipes without letting either fill up. */
		int open_o = 1, open_e = 1;
		char buf[4096];
		while (open_o || open_e) {
			fd_set rs;
			FD_ZERO(&rs);
			if (open_o)
				FD_SET(po[0], &rs);
			if (open_e)
				FD_SET(pe[0], &rs);
			int m = po[0] > pe[0] ? po[0] : pe[0];
			if (select(m + 1, &rs, NULL, NULL, NULL) < 0) {
				if (errno == EINTR)
					continue;
				break;
			}
			if (open_o && FD_ISSET(po[0], &rs)) {
				ssize_t n = read(po[0], buf, sizeof buf);
				if (n <= 0)
					open_o = 0;
				else
					kn_string_push(out, (const uint8_t *)buf, (size_t)n);
			}
			if (open_e && FD_ISSET(pe[0], &rs)) {
				ssize_t n = read(pe[0], buf, sizeof buf);
				if (n <= 0)
					open_e = 0;
				else
					kn_string_push(err, (const uint8_t *)buf, (size_t)n);
			}
		}
		close(po[0]);
		close(pe[0]);
		po[0] = pe[0] = -1;
	}
	int status = 0;
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {
	}
	if (got == (ssize_t)sizeof child_errno) {
		result = mkerr(kind_of_errno(child_errno), child_errno);
	} else if (WIFEXITED(status)) {
		result = WEXITSTATUS(status);
	} else {
		result = 128 + WTERMSIG(status);
	}
done:
	if (capture) {
		if (po[0] >= 0)
			close(po[0]);
		if (pe[0] >= 0)
			close(pe[0]);
	}
#endif
	for (size_t i = 0; i < argc; i++)
		free(args[i]);
	free(args);
	return result;
}

/* ----- UTF-8 -------------------------------------------------------------------- */

/* Length of the longest valid UTF-8 prefix of (p, n). */
int64_t kn_utf8_valid_up_to(const uint8_t *p, size_t n) {
	size_t i = 0;
	while (i < n) {
		uint8_t c = p[i];
		size_t len;
		uint32_t cp;
		if (c < 0x80) {
			i++;
			continue;
		} else if ((c & 0xE0) == 0xC0) {
			len = 2;
			cp = c & 0x1F;
		} else if ((c & 0xF0) == 0xE0) {
			len = 3;
			cp = c & 0x0F;
		} else if ((c & 0xF8) == 0xF0) {
			len = 4;
			cp = c & 0x07;
		} else {
			return (int64_t)i;
		}
		if (i + len > n)
			return (int64_t)i;
		for (size_t j = 1; j < len; j++) {
			if ((p[i + j] & 0xC0) != 0x80)
				return (int64_t)i;
			cp = (cp << 6) | (p[i + j] & 0x3F);
		}
		if ((len == 2 && cp < 0x80) || (len == 3 && cp < 0x800) || (len == 4 && (cp < 0x10000 || cp > 0x10FFFF)) ||
		    (cp >= 0xD800 && cp <= 0xDFFF))
			return (int64_t)i;
		i += len;
	}
	return (int64_t)n;
}
