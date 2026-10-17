/*
 * The Kiln runtime library.
 *
 * Compiled once when the `kiln` tool is built and linked into every Kiln
 * executable. Generated code calls these functions for allocation, panics,
 * printing, string formatting, and growable buffers. Everything else (drop
 * glue, Vec/String layout, vtables) is emitted by the code generator.
 *
 * Layouts shared with the code generator (64-bit targets):
 *   String, Vec<T>:  { uint8_t *ptr; size_t cap; size_t len; }   (24 bytes)
 *   &str, &[T]:      { const uint8_t *ptr; size_t len; }          (16 bytes)
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#ifndef _MSC_VER
#include <unistd.h>
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

typedef struct {
	const uint8_t *ptr;
	size_t len;
} KnStr;

/* ----- memory ------------------------------------------------------------ */

KN_NORETURN void kn_panic(const uint8_t *msg, size_t len);

static KN_NORETURN void oom(void) {
	static const char m[] = "out of memory";
	kn_panic((const uint8_t *)m, sizeof m - 1);
}

void *kn_alloc(size_t size, size_t align) {
	(void)align; /* malloc is aligned for every Kiln type (max align 8) */
	if (size == 0)
		return (void *)(uintptr_t)8;
	void *p = malloc(size);
	if (!p)
		oom();
	return p;
}

void kn_free(void *p, size_t size, size_t align) {
	(void)align;
	if (size != 0 && p)
		free(p);
}

/* ----- panics and printing ----------------------------------------------- */

/* Frees a closure environment from kn_alloc (size 0 environments are dangling). */
void kn_free_env(void *env) {
	if (env && env != (void *)(uintptr_t)8)
		free(env);
}

#ifdef _MSC_VER
#include <windows.h>
static volatile LONG panicking;
static int first_panic(void) { return InterlockedExchange(&panicking, 1) == 0; }
#define lock_file(f) _lock_file(f)
#define unlock_file(f) _unlock_file(f)
#else
static volatile int panicking;
static int first_panic(void) { return __atomic_exchange_n(&panicking, 1, __ATOMIC_SEQ_CST) == 0; }
#define lock_file(f) flockfile(f)
#define unlock_file(f) funlockfile(f)
#endif

/* Prints `panic: <msg>` and exits with status 101. If several tasks panic
 * at once, the first one reports and the others wait to exit. */
static KN_NORETURN void panic_die(const uint8_t *msg, size_t len) {
	if (!first_panic())
		for (;;) {
#ifdef _MSC_VER
			Sleep(1000);
#else
			sleep(1);
#endif
		}
	fflush(stdout);
	fputs("panic: ", stderr);
	fwrite(msg, 1, len, stderr);
	fputc('\n', stderr);
	fflush(stderr);
	exit(101);
}

/* ----- running `defer` on panic ---------------------------------------------
 *
 * There is no unwinding. A function that uses `defer` registers a frame
 * (kn_frame_enter) and calls setjmp itself. A panic jumps to the innermost
 * frame, whose code runs its pending deferred calls through the normal
 * dispatch, and kn_frame_leave passes the panic on to the next frame. After
 * the last frame, the message is printed and the process exits, as in Go
 * (without `recover`). Locals are not dropped on this path. */

typedef struct KnPanicFrame {
	struct KnPanicFrame *prev;
	void *pad;
	/* jmp_buf follows (the generated code reserves room for it) */
} KnPanicFrame;

typedef struct {
	KnPanicFrame *head;
	KnPanicFrame *landed; /* the frame running its defers for a panic */
	uint8_t *msg;
	size_t len;
	int unwinding;
} KnPanicState;

void *kn_sched_panic_state(void); /* kiln_sched.c: per task */

static KnPanicState *panic_state(void) { return (KnPanicState *)kn_sched_panic_state(); }

#if !defined(_WIN32)
#include <setjmp.h>
static KN_NORETURN void unwind_next(KnPanicState *ps) {
	KnPanicFrame *f = ps->head;
	if (f) {
		ps->head = f->prev;
		ps->landed = f;
		_longjmp(*(jmp_buf *)((char *)f + sizeof(KnPanicFrame)), 1);
	}
	panic_die(ps->msg, ps->len);
}
#endif

/* Links `f` (in the caller's frame) and returns its jmp_buf. */
void *kn_frame_enter(KnPanicFrame *f) {
	KnPanicState *ps = panic_state();
	f->prev = ps->head;
	ps->head = f;
	return (char *)f + sizeof(KnPanicFrame);
}

/* The function's defers are done: unlink `f`, or keep panicking. */
void kn_frame_leave(KnPanicFrame *f) {
	KnPanicState *ps = panic_state();
#if !defined(_WIN32)
	if (ps->unwinding && ps->landed == f)
		unwind_next(ps);
#endif
	ps->head = f->prev;
}

KN_NORETURN void kn_panic(const uint8_t *msg, size_t len) {
#if !defined(_WIN32)
	KnPanicState *ps = panic_state();
	if (ps && ps->head) {
		if (!ps->unwinding) {
			/* Keep the first message; a panic inside a deferred call
			 * just moves on to the next frame. */
			ps->msg = (uint8_t *)malloc(len ? len : 1);
			if (ps->msg)
				memcpy(ps->msg, msg, len);
			ps->len = ps->msg ? len : 0;
			ps->unwinding = 1;
		}
		unwind_next(ps);
	}
	if (ps && ps->unwinding)
		panic_die(ps->msg, ps->len);
#endif
	panic_die(msg, len);
}

KN_NORETURN void kn_panic_bounds(size_t idx, size_t len) {
	char buf[128];
	int n = snprintf(buf, sizeof buf, "index out of bounds: the length is %zu but the index is %zu", len, idx);
	kn_panic((const uint8_t *)buf, (size_t)n);
}

KN_NORETURN void kn_panic_str(const char *msg) {
	kn_panic((const uint8_t *)msg, strlen(msg));
}

void kn_print(const uint8_t *p, size_t len, int32_t to_stderr, int32_t newline) {
	FILE *f = to_stderr ? stderr : stdout;
	if (to_stderr)
		fflush(stdout);
	/* One line at a time, even with tasks printing on several threads. */
	lock_file(f);
	fwrite(p, 1, len, f);
	if (newline)
		fputc('\n', f);
	unlock_file(f);
}

/* Prints `Error: <msg>` when `main` returns `Err`. */
void kn_main_error(const uint8_t *p, size_t len) {
	fflush(stdout);
	fputs("Error: ", stderr);
	fwrite(p, 1, len, stderr);
	fputc('\n', stderr);
}

/* ----- growable buffers (Vec<T>, String) -------------------------------- */

void kn_vec_reserve(KnVec *v, size_t additional, size_t elem, size_t align) {
	(void)align;
	if (v->cap - v->len >= additional)
		return;
	size_t want = v->len + additional;
	size_t cap = v->cap ? v->cap * 2 : 4;
	if (cap < want)
		cap = want;
	if (elem == 0) {
		v->cap = SIZE_MAX;
		return;
	}
	void *p = v->cap ? realloc(v->ptr, cap * elem) : malloc(cap * elem);
	if (!p)
		oom();
	v->ptr = p;
	v->cap = cap;
}

void kn_vec_free(KnVec *v, size_t elem, size_t align) {
	(void)align;
	if (v->cap && elem && v->cap != SIZE_MAX)
		free(v->ptr);
}

void kn_string_push(KnVec *s, const uint8_t *p, size_t len) {
	kn_vec_reserve(s, len, 1, 1);
	if (len)
		memcpy(s->ptr + s->len, p, len);
	s->len += len;
}

void kn_string_from(KnVec *out, const uint8_t *p, size_t len) {
	out->ptr = (uint8_t *)(uintptr_t)1;
	out->cap = 0;
	out->len = 0;
	kn_string_push(out, p, len);
}

/* ----- formatting -------------------------------------------------------- */

/* Decimal digits of `v` at the end of buf[0..24); returns where they start. */
static char *u64_digits(char *end, uint64_t v) {
	char *p = end;
	do {
		*--p = (char)('0' + v % 10);
		v /= 10;
	} while (v);
	return p;
}

void kn_fmt_i64(KnVec *s, int64_t v) {
	char buf[24];
	char *end = buf + sizeof buf;
	uint64_t u = v < 0 ? 0 - (uint64_t)v : (uint64_t)v;
	char *p = u64_digits(end, u);
	if (v < 0)
		*--p = '-';
	kn_string_push(s, (const uint8_t *)p, (size_t)(end - p));
}

void kn_fmt_u64(KnVec *s, uint64_t v) {
	char buf[24];
	char *end = buf + sizeof buf;
	char *p = u64_digits(end, v);
	kn_string_push(s, (const uint8_t *)p, (size_t)(end - p));
}

/* 128-bit integers arrive as two 64-bit halves (the same in every backend). */
static char *u128_digits(char *end, unsigned __int128 v) {
	char *p = end;
	/* Peel 19 digits at a time so most of the work is 64-bit division. */
	while (v > UINT64_MAX) {
		uint64_t chunk = (uint64_t)(v % 10000000000000000000ULL);
		v /= 10000000000000000000ULL;
		for (int i = 0; i < 19; i++) {
			*--p = (char)('0' + chunk % 10);
			chunk /= 10;
		}
	}
	return u64_digits(p, (uint64_t)v);
}

void kn_fmt_u128(KnVec *s, uint64_t lo, uint64_t hi) {
	char buf[48];
	char *end = buf + sizeof buf;
	char *p = u128_digits(end, ((unsigned __int128)hi << 64) | lo);
	kn_string_push(s, (const uint8_t *)p, (size_t)(end - p));
}

void kn_fmt_i128(KnVec *s, uint64_t lo, uint64_t hi) {
	char buf[48];
	char *end = buf + sizeof buf;
	unsigned __int128 u = ((unsigned __int128)hi << 64) | lo;
	int neg = (int64_t)hi < 0;
	char *p = u128_digits(end, neg ? 0 - u : u);
	if (neg)
		*--p = '-';
	kn_string_push(s, (const uint8_t *)p, (size_t)(end - p));
}

/* 128-bit division for the Cranelift backend, which has no native
 * i128 div/rem on x86-64. flags: bit 0 = remainder, bit 1 = signed.
 * The divisor is already known to be nonzero. */
void kn_divrem128(unsigned __int128 *out, uint64_t alo, uint64_t ahi, uint64_t blo, uint64_t bhi,
		  uint64_t flags) {
	unsigned __int128 a = ((unsigned __int128)ahi << 64) | alo;
	unsigned __int128 b = ((unsigned __int128)bhi << 64) | blo;
	int rem = flags & 1;
	if (flags & 2) {
		__int128 sa = (__int128)a, sb = (__int128)b;
		if (sb == -1 && a == (unsigned __int128)1 << 127)
			kn_panic_str(rem ? "attempt to calculate the remainder with overflow"
					 : "attempt to divide with overflow");
		*out = (unsigned __int128)(rem ? sa % sb : sa / sb);
	} else {
		*out = rem ? a % b : a / b;
	}
}

/* 128-bit multiply with overflow (`overflowing_mul`): stores the wrapped
 * product and returns 1 if it overflowed. */
uint64_t kn_mulo128(unsigned __int128 *out, uint64_t alo, uint64_t ahi, uint64_t blo, uint64_t bhi,
		    uint64_t is_signed) {
	unsigned __int128 a = ((unsigned __int128)ahi << 64) | alo;
	unsigned __int128 b = ((unsigned __int128)bhi << 64) | blo;
	if (is_signed) {
		__int128 r;
		int o = __builtin_mul_overflow((__int128)a, (__int128)b, &r);
		*out = (unsigned __int128)r;
		return (uint64_t)o;
	}
	unsigned __int128 r;
	int o = __builtin_mul_overflow(a, b, &r);
	*out = r;
	return (uint64_t)o;
}

/* i128 <-> f64 for the Cranelift backend. Float to int saturates (and NaN
 * is 0), like `as`. */
double kn_i128_to_f64(uint64_t lo, uint64_t hi, uint64_t is_signed) {
	unsigned __int128 v = ((unsigned __int128)hi << 64) | lo;
	return is_signed ? (double)(__int128)v : (double)v;
}

void kn_f64_to_i128(unsigned __int128 *out, double f, uint64_t is_signed) {
	if (f != f) {
		*out = 0;
	} else if (is_signed) {
		__int128 max = (__int128)(((unsigned __int128)1 << 127) - 1);
		if (f >= 0x1p127)
			*out = (unsigned __int128)max;
		else if (f <= -0x1p127)
			*out = (unsigned __int128)(-max - 1);
		else
			*out = (unsigned __int128)(__int128)f;
	} else {
		if (f >= 0x1p128)
			*out = ~(unsigned __int128)0;
		else if (f <= 0)
			*out = 0;
		else
			*out = (unsigned __int128)f;
	}
}

void kn_fmt_bool(KnVec *s, int32_t b) {
	if (b)
		kn_string_push(s, (const uint8_t *)"true", 4);
	else
		kn_string_push(s, (const uint8_t *)"false", 5);
}

void kn_fmt_char(KnVec *s, uint32_t c) {
	uint8_t b[4];
	size_t n;
	if (c < 0x80) {
		b[0] = (uint8_t)c;
		n = 1;
	} else if (c < 0x800) {
		b[0] = (uint8_t)(0xC0 | (c >> 6));
		b[1] = (uint8_t)(0x80 | (c & 0x3F));
		n = 2;
	} else if (c < 0x10000) {
		b[0] = (uint8_t)(0xE0 | (c >> 12));
		b[1] = (uint8_t)(0x80 | ((c >> 6) & 0x3F));
		b[2] = (uint8_t)(0x80 | (c & 0x3F));
		n = 3;
	} else {
		b[0] = (uint8_t)(0xF0 | (c >> 18));
		b[1] = (uint8_t)(0x80 | ((c >> 12) & 0x3F));
		b[2] = (uint8_t)(0x80 | ((c >> 6) & 0x3F));
		b[3] = (uint8_t)(0x80 | (c & 0x3F));
		n = 4;
	}
	kn_string_push(s, b, n);
}

/* Like Rust's `Display` for floats: the shortest digits that round-trip,
 * written without an exponent (`1`, `0.1`, `2.5`, `100000`, `0.00001`). */
void kn_ryu_digits(double f, uint64_t *mantissa, int32_t *exponent);

void kn_fmt_f64(KnVec *s, double v) {
	if (isnan(v)) {
		kn_string_push(s, (const uint8_t *)"NaN", 3);
		return;
	}
	if (isinf(v)) {
		if (v < 0)
			kn_string_push(s, (const uint8_t *)"-inf", 4);
		else
			kn_string_push(s, (const uint8_t *)"inf", 3);
		return;
	}
	/* Fast path: a value that is exactly t / 10^k for an integer t below
	 * 2^53 and k <= 3 (whole numbers, money-like decimals). At that
	 * magnitude doubles are far denser than 10^-3, so the digits of t
	 * (trailing zeros dropped) are the shortest round-trip form. */
	{
		double a = fabs(v), sc = 1;
		for (int k = 0; k <= 3; k++, sc *= 10) {
			double t = a * sc;
			if (t >= 9007199254740992.0 || (k > 0 && a < 1e-3))
				break;
			if (t == (double)(uint64_t)t && (double)(uint64_t)t / sc == a) {
				uint64_t m = (uint64_t)t;
				char buf[32];
				int i = sizeof buf;
				for (int d = 0; d < k; d++) {
					buf[--i] = (char)('0' + m % 10);
					m /= 10;
				}
				if (k > 0)
					buf[--i] = '.';
				do {
					buf[--i] = (char)('0' + m % 10);
					m /= 10;
				} while (m);
				if (signbit(v))
					buf[--i] = '-';
				kn_string_push(s, (const uint8_t *)buf + i, sizeof buf - (size_t)i);
				return;
			}
		}
	}
	/* Shortest round-trip digits (Ryu): |v| = mant * 10^e10. */
	char digits[32];
	int nd = 0, neg = signbit(v) != 0, exp;
	if (v == 0) {
		digits[nd++] = '0';
		exp = 0;
	} else {
		uint64_t mant;
		int32_t e10;
		kn_ryu_digits(v, &mant, &e10);
		char tmp[24];
		int t = 0;
		while (mant) {
			tmp[t++] = (char)('0' + mant % 10);
			mant /= 10;
		}
		while (t)
			digits[nd++] = tmp[--t];
		exp = e10 + nd - 1;
	}
	char out[400];
	int o = 0;
	if (neg)
		out[o++] = '-';
	int point = exp + 1; /* digits before the decimal point */
	if (point <= 0) {
		out[o++] = '0';
		out[o++] = '.';
		for (int k = 0; k < -point; k++)
			out[o++] = '0';
		for (int k = 0; k < nd; k++)
			out[o++] = digits[k];
	} else {
		for (int k = 0; k < point; k++)
			out[o++] = k < nd ? digits[k] : '0';
		if (nd > point) {
			out[o++] = '.';
			for (int k = point; k < nd; k++)
				out[o++] = digits[k];
		}
	}
	/* Like Rust, negative zero prints as "-0". */
	kn_string_push(s, (const uint8_t *)out, (size_t)o);
}

/* ----- strings ----------------------------------------------------------- */

int32_t kn_str_eq(const uint8_t *a, size_t al, const uint8_t *b, size_t bl) {
	return al == bl && (al == 0 || memcmp(a, b, al) == 0);
}

int32_t kn_str_cmp(const uint8_t *a, size_t al, const uint8_t *b, size_t bl) {
	size_t n = al < bl ? al : bl;
	int c = n ? memcmp(a, b, n) : 0;
	if (c)
		return c < 0 ? -1 : 1;
	return al < bl ? -1 : (al > bl ? 1 : 0);
}

void kn_str_to_upper(KnVec *out, const uint8_t *p, size_t len) {
	kn_string_from(out, p, len);
	for (size_t i = 0; i < len; i++)
		if (out->ptr[i] >= 'a' && out->ptr[i] <= 'z')
			out->ptr[i] -= 32;
}

void kn_str_to_lower(KnVec *out, const uint8_t *p, size_t len) {
	kn_string_from(out, p, len);
	for (size_t i = 0; i < len; i++)
		if (out->ptr[i] >= 'A' && out->ptr[i] <= 'Z')
			out->ptr[i] += 32;
}

static int is_space(uint8_t c) {
	return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f';
}

void kn_str_trim(KnStr *out, const uint8_t *p, size_t len) {
	size_t a = 0, b = len;
	while (a < b && is_space(p[a]))
		a++;
	while (b > a && is_space(p[b - 1]))
		b--;
	out->ptr = p + a;
	out->len = b - a;
}

int32_t kn_str_starts_with(const uint8_t *p, size_t len, const uint8_t *q, size_t ql) {
	return ql <= len && (ql == 0 || memcmp(p, q, ql) == 0);
}

int32_t kn_str_ends_with(const uint8_t *p, size_t len, const uint8_t *q, size_t ql) {
	return ql <= len && (ql == 0 || memcmp(p + len - ql, q, ql) == 0);
}

int32_t kn_str_contains(const uint8_t *p, size_t len, const uint8_t *q, size_t ql) {
	if (ql == 0)
		return 1;
	for (size_t i = 0; i + ql <= len; i++)
		if (memcmp(p + i, q, ql) == 0)
			return 1;
	return 0;
}

/* ----- defer stacks ------------------------------------------------------ */

/* A function's defer stack: (call id, saved arguments) pairs, plus the
 * arguments of the call being run. */
typedef struct {
	KnVec entries;
	void *current;
} KnDefer;

typedef struct {
	uint64_t id;
	void *args;
} KnDeferEntry;

void kn_defer_push(KnDefer *d, uint32_t id, void *args) {
	kn_vec_reserve(&d->entries, 1, sizeof(KnDeferEntry), 8);
	KnDeferEntry *e = &((KnDeferEntry *)d->entries.ptr)[d->entries.len++];
	e->id = id;
	e->args = args;
}

/* Pops the most recent deferred call: returns its id and sets `current` to
 * its saved arguments, or returns -1 (and frees the stack). */
int64_t kn_defer_pop(KnDefer *d) {
	if (d->entries.len == 0) {
		kn_vec_free(&d->entries, sizeof(KnDeferEntry), 8);
		d->entries.cap = 0;
		return -1;
	}
	KnDeferEntry e = ((KnDeferEntry *)d->entries.ptr)[--d->entries.len];
	d->current = e.args;
	return (int64_t)e.id;
}

/* ----- math -------------------------------------------------------------- */

double kn_round(double v) { return round(v); }
double kn_powi(double v, int32_t n) { return pow(v, (double)n); }
double kn_fmod(double a, double b) { return fmod(a, b); }

/* ----- slicing ----------------------------------------------------------- */

/* Checks `start..end` against `len` for `&x[start..end]` and returns the
 * exclusive end. flags: bit 0 = has end (else `len`), bit 1 = inclusive.
 * For strings, both ends must fall on UTF-8 character boundaries. */
int64_t kn_slice_check(const uint8_t *data, int64_t len, int64_t start, int64_t end, int64_t flags, int64_t is_str) {
	if (!(flags & 1))
		end = len;
	else if (flags & 2)
		end += 1;
	if ((uint64_t)start > (uint64_t)end) {
		char msg[96];
		snprintf(msg, sizeof msg, "slice index starts at %lld but ends at %lld", (long long)start, (long long)end);
		kn_panic_str(msg);
	}
	if ((uint64_t)end > (uint64_t)len) {
		char msg[96];
		snprintf(msg, sizeof msg, "range end index %lld out of range for slice of length %lld", (long long)end, (long long)len);
		kn_panic_str(msg);
	}
	if (is_str) {
		if ((start < len && (data[start] & 0xC0) == 0x80) || (end < len && (data[end] & 0xC0) == 0x80))
			kn_panic_str("byte index is not a char boundary");
	}
	return end;
}

/* `ptr::copy`: overlapping copies are fine. */
void kn_memmove(void *dst, const void *src, size_t n) {
	if (n)
		memmove(dst, src, n);
}

#if defined(_MSC_VER) && !defined(__clang__)
#include <intrin.h>
#endif

/* ---- Hashing (std::hash::Hasher) ----
 * A fast multiply-fold hash (in the style of wyhash/foldhash) with a random
 * per-process seed, so hash tables resist inputs built to collide. The state
 * is one u64; every input is mixed in with a 64x64->128 multiply. */
static inline uint64_t kn_fold_mul(uint64_t a, uint64_t b) {
#if defined(_MSC_VER) && !defined(__clang__)
    return (a * b) ^ __umulh(a, b);
#else
    __uint128_t p = (__uint128_t)a * b;
    return (uint64_t)p ^ (uint64_t)(p >> 64);
#endif
}

#define KN_H1 0x243f6a8885a308d3ULL
#define KN_H2 0x13198a2e03707344ULL

void kn_hash_u64(uint64_t *h, int64_t x) {
    *h = kn_fold_mul(*h ^ (uint64_t)x, KN_H1);
}

void kn_hash_bytes(uint64_t *h, const uint8_t *p, size_t n) {
    uint64_t s = *h;
    size_t i = 0;
    for (; i + 16 <= n; i += 16) {
        uint64_t a, b;
        memcpy(&a, p + i, 8);
        memcpy(&b, p + i + 8, 8);
        s = kn_fold_mul(s ^ a, b ^ KN_H1);
    }
    if (i + 8 <= n) {
        uint64_t a;
        memcpy(&a, p + i, 8);
        s = kn_fold_mul(s ^ a, KN_H2);
        i += 8;
    }
    uint64_t tail = 0;
    if (i < n) memcpy(&tail, p + i, n - i);
    /* The length is mixed in, so "ab"+"c" and "a"+"bc" differ. */
    *h = kn_fold_mul(s ^ tail, KN_H1 ^ (uint64_t)n);
}

uint64_t kn_hash_finish(uint64_t h) {
    return kn_fold_mul(h, KN_H2) ^ h;
}

/* Whether NUL-terminated a equals b ignoring ASCII case. */
static int strcasecmp_ascii(const char *a, const char *b) {
    for (; *a && *b; a++, b++) {
        char x = *a >= 'A' && *a <= 'Z' ? *a + 32 : *a;
        if (x != *b) return 0;
    }
    return *a == 0 && *b == 0;
}

/* ---- Number parsing (std::strconv) ----
 * Parses a decimal floating-point number: [+-] digits [. digits] [e [+-] digits],
 * or inf / infinity / nan (any case). The whole input must match. Returns 1 and
 * stores the correctly rounded value on success, 0 on bad syntax. The program
 * never calls setlocale, so strtod uses '.' as the decimal point. */
int32_t kn_parse_f64(const uint8_t *p, size_t n, double *out) {
    size_t i = 0;
    char buf[512];
    if (n == 0 || n >= sizeof buf) {
        if (n >= sizeof buf) {
            /* Long inputs (many digits) are valid; parse from a heap copy. */
            char *h = malloc(n + 1);
            if (!h) return 0;
            memcpy(h, p, n);
            h[n] = 0;
            int32_t ok = 0;
            size_t j = 0;
            if (h[j] == '+' || h[j] == '-') j++;
            size_t d = 0;
            while (j < n && h[j] >= '0' && h[j] <= '9') j++, d++;
            if (j < n && h[j] == '.') {
                j++;
                while (j < n && h[j] >= '0' && h[j] <= '9') j++, d++;
            }
            if (d > 0 && j < n && (h[j] == 'e' || h[j] == 'E')) {
                j++;
                if (j < n && (h[j] == '+' || h[j] == '-')) j++;
                size_t e = 0;
                while (j < n && h[j] >= '0' && h[j] <= '9') j++, e++;
                if (e == 0) d = 0;
            }
            if (d > 0 && j == n) {
                *out = strtod(h, NULL);
                ok = 1;
            }
            free(h);
            return ok;
        }
        return 0;
    }
    memcpy(buf, p, n);
    buf[n] = 0;
    if (buf[i] == '+' || buf[i] == '-') i++;
    const char *rest = buf + i;
    if (strcasecmp_ascii(rest, "inf") || strcasecmp_ascii(rest, "infinity")) {
        *out = buf[0] == '-' ? -INFINITY : INFINITY;
        return 1;
    }
    if (strcasecmp_ascii(rest, "nan")) {
        *out = NAN;
        return 1;
    }
    size_t digits = 0;
    while (i < n && buf[i] >= '0' && buf[i] <= '9') i++, digits++;
    if (i < n && buf[i] == '.') {
        i++;
        while (i < n && buf[i] >= '0' && buf[i] <= '9') i++, digits++;
    }
    if (digits == 0) return 0;
    if (i < n && (buf[i] == 'e' || buf[i] == 'E')) {
        i++;
        if (i < n && (buf[i] == '+' || buf[i] == '-')) i++;
        size_t e = 0;
        while (i < n && buf[i] >= '0' && buf[i] <= '9') i++, e++;
        if (e == 0) return 0;
    }
    if (i != n) return 0;
    *out = strtod(buf, NULL);
    return 1;
}

/* ---- Formatting (format specs and Debug) ----
 * KnFormatter mirrors std::fmt::Formatter: { out: String, depth: usize, pretty: bool }. */
typedef struct {
    KnVec out;
    uint64_t depth;
    uint8_t pretty;
} KnFormatter;

static void kn_push_cstr(KnVec *s, const char *c) {
    kn_string_push(s, (const uint8_t *)c, strlen(c));
}

static void kn_push_utf8(KnVec *s, uint32_t c) {
    uint8_t b[4];
    size_t n;
    if (c < 0x80) {
        b[0] = (uint8_t)c;
        n = 1;
    } else if (c < 0x800) {
        b[0] = (uint8_t)(0xC0 | (c >> 6));
        b[1] = (uint8_t)(0x80 | (c & 63));
        n = 2;
    } else if (c < 0x10000) {
        b[0] = (uint8_t)(0xE0 | (c >> 12));
        b[1] = (uint8_t)(0x80 | ((c >> 6) & 63));
        b[2] = (uint8_t)(0x80 | (c & 63));
        n = 3;
    } else {
        b[0] = (uint8_t)(0xF0 | (c >> 18));
        b[1] = (uint8_t)(0x80 | ((c >> 12) & 63));
        b[2] = (uint8_t)(0x80 | ((c >> 6) & 63));
        b[3] = (uint8_t)(0x80 | (c & 63));
        n = 4;
    }
    kn_string_push(s, b, n);
}

/* Escapes one character for Debug output inside `quote` quotes. */
static void kn_escape_char(KnVec *s, uint32_t c, char quote) {
    char buf[16];
    switch (c) {
    case '\n': kn_push_cstr(s, "\\n"); return;
    case '\r': kn_push_cstr(s, "\\r"); return;
    case '\t': kn_push_cstr(s, "\\t"); return;
    case '\\': kn_push_cstr(s, "\\\\"); return;
    case 0: kn_push_cstr(s, "\\0"); return;
    }
    if (c == (uint32_t)quote) {
        buf[0] = '\\';
        buf[1] = quote;
        kn_string_push(s, (const uint8_t *)buf, 2);
        return;
    }
    if (c < 32 || c == 127) {
        snprintf(buf, sizeof buf, "\\u{%x}", (unsigned)c);
        kn_push_cstr(s, buf);
        return;
    }
    kn_push_utf8(s, c);
}

/* Debug form of a string: quoted, with escapes. */
void kn_fmt_debug_str(KnVec *s, const uint8_t *p, size_t n) {
    kn_push_cstr(s, "\"");
    size_t i = 0;
    while (i < n) {
        uint8_t b = p[i];
        uint32_t c;
        size_t k;
        if (b < 0x80) {
            c = b;
            k = 1;
        } else if ((b & 0xE0) == 0xC0) {
            c = ((uint32_t)(b & 31) << 6) | (p[i + 1] & 63);
            k = 2;
        } else if ((b & 0xF0) == 0xE0) {
            c = ((uint32_t)(b & 15) << 12) | ((uint32_t)(p[i + 1] & 63) << 6) | (p[i + 2] & 63);
            k = 3;
        } else {
            c = ((uint32_t)(b & 7) << 18) | ((uint32_t)(p[i + 1] & 63) << 12) |
                ((uint32_t)(p[i + 2] & 63) << 6) | (p[i + 3] & 63);
            k = 4;
        }
        kn_escape_char(s, c, '"');
        i += k;
    }
    kn_push_cstr(s, "\"");
}

/* Debug form of a char: 'c' with escapes. */
void kn_fmt_debug_char(KnVec *s, uint32_t c) {
    kn_push_cstr(s, "'");
    kn_escape_char(s, c, '\'');
    kn_push_cstr(s, "'");
}

void kn_fmt_f64_exp(KnVec *s, double v, int64_t prec, int32_t upper);

/* Debug form of a float, like Rust: shortest exponent form outside
 * 1e-4 <= |v| < 1e16 (`1e16`, `6.9e-5`), otherwise like Display but whole
 * numbers keep ".0". */
void kn_fmt_debug_f64(KnVec *s, double v) {
    double a = fabs(v);
    if (!isnan(v) && !isinf(v) && a != 0 && (a < 1e-4 || a >= 1e16)) {
        kn_fmt_f64_exp(s, v, -1, 0);
        return;
    }
    size_t start = s->len;
    kn_fmt_f64(s, v);
    if (isnan(v) || isinf(v)) return;
    for (size_t i = start; i < s->len; i++)
        if (s->ptr[i] == '.' || s->ptr[i] == 'e') return;
    kn_push_cstr(s, ".0");
}

/* `v` in base 2, 8, or 16; `alt` adds the 0b/0o/0x prefix. */
void kn_fmt_radix(KnVec *s, uint64_t v, int32_t radix, int32_t upper, int32_t alt) {
    char buf[80];
    int i = sizeof buf;
    const char *dig = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    do {
        buf[--i] = dig[v % (uint64_t)radix];
        v /= (uint64_t)radix;
    } while (v);
    if (alt) {
        buf[--i] = radix == 16 ? 'x' : radix == 8 ? 'o' : 'b';
        buf[--i] = '0';
    }
    kn_string_push(s, (const uint8_t *)buf + i, sizeof buf - (size_t)i);
}

/* `v` with exactly `prec` digits after the point. */
void kn_fmt_f64_fixed(KnVec *s, double v, int64_t prec) {
    if (isnan(v) || isinf(v)) {
        kn_fmt_f64(s, v);
        return;
    }
    if (prec > 1000) prec = 1000;
    int n = snprintf(NULL, 0, "%.*f", (int)prec, v);
    char *buf = malloc((size_t)n + 1);
    if (!buf) oom();
    snprintf(buf, (size_t)n + 1, "%.*f", (int)prec, v);
    kn_string_push(s, (const uint8_t *)buf, (size_t)n);
    free(buf);
}

/* Scientific notation like Rust: 1.5e3, 1e-7, 2.50E10 (prec < 0: shortest). */
void kn_fmt_f64_exp(KnVec *s, double v, int64_t prec, int32_t upper) {
    if (isnan(v) || isinf(v)) {
        kn_fmt_f64(s, v);
        return;
    }
    char buf[64];
    if (prec < 0) {
        int p;
        for (p = 1; p <= 17; p++) {
            snprintf(buf, sizeof buf, "%.*e", p - 1, v);
            if (strtod(buf, NULL) == v) break;
        }
    } else {
        if (prec > 40) prec = 40;
        snprintf(buf, sizeof buf, "%.*e", (int)prec, v);
    }
    char *e = strchr(buf, 'e');
    if (!e) {
        kn_push_cstr(s, buf);
        return;
    }
    *e = 0;
    int exp = atoi(e + 1);
    /* Shortest form: drop a trailing ".0..." (1.0e3 -> 1e3). */
    if (prec < 0) {
        char *dot = strchr(buf, '.');
        if (dot) {
            char *end = buf + strlen(buf);
            while (end > dot + 1 && end[-1] == '0') *--end = 0;
            if (end == dot + 1) *dot = 0;
        }
    }
    char tail[16];
    snprintf(tail, sizeof tail, "%c%d", upper ? 'E' : 'e', exp);
    kn_push_cstr(s, buf);
    kn_push_cstr(s, tail);
}

static void kn_indent(KnVec *s, uint64_t depth) {
    for (uint64_t i = 0; i < depth; i++) kn_push_cstr(s, "    ");
}

/* Opens a list, tuple, or struct body: writes `open`, one level deeper. */
void kn_fmt_begin(KnFormatter *f, const uint8_t *p, size_t n) {
    kn_string_push(&f->out, p, n);
    f->depth++;
}

/* Before an item: a separator (and in pretty mode, a newline and indent).
 * `spaced` puts a space after `{` in compact mode: `P { x: 1 }`. */
void kn_fmt_sep(KnFormatter *f, int32_t first, int32_t spaced) {
    if (f->pretty) {
        kn_push_cstr(&f->out, first ? "\n" : ",\n");
        kn_indent(&f->out, f->depth);
    } else if (!first) {
        kn_push_cstr(&f->out, ", ");
    } else if (spaced) {
        kn_push_cstr(&f->out, " ");
    }
}

/* Closes what kn_fmt_begin opened; `empty` if no items were written. */
void kn_fmt_end(KnFormatter *f, const uint8_t *p, size_t n, int32_t empty, int32_t spaced) {
    f->depth--;
    if (!empty) {
        if (f->pretty) {
            kn_push_cstr(&f->out, ",\n");
            kn_indent(&f->out, f->depth);
        } else if (spaced) {
            kn_push_cstr(&f->out, " ");
        }
    }
    kn_string_push(&f->out, p, n);
}

/* Appends `src` to `dst`, padded to `width` characters with `fill`.
 * align: 0 default, 1 left, 2 center, 3 right. flags: 1 plus sign, 2 zero
 * padding, 4 numeric (right-aligned by default; signs/prefixes stay in front
 * of zero padding). `prec` >= 0 truncates non-numeric text to that many chars. */
void kn_fmt_pad(KnVec *dst, const uint8_t *src, size_t n, int64_t width, uint32_t fill,
                int32_t align, int32_t flags, int64_t prec) {
    int numeric = (flags & 4) != 0;
    const char *sign = "";
    if (numeric && (flags & 1) && !(n > 0 && src[0] == '-') &&
        !(n >= 3 && (memcmp(src, "NaN", 3) == 0))) {
        sign = "+";
    }
    if (!numeric && prec >= 0) {
        /* Truncate to prec characters. */
        size_t i = 0;
        int64_t c = 0;
        while (i < n && c < prec) {
            i++;
            while (i < n && (src[i] & 0xC0) == 0x80) i++;
            c++;
        }
        n = i;
    }
    int64_t chars = (int64_t)strlen(sign);
    for (size_t i = 0; i < n; i++)
        if ((src[i] & 0xC0) != 0x80) chars++;
    int64_t pad = width > chars ? width - chars : 0;
    if (numeric && (flags & 2)) {
        /* Zero padding goes after the sign and any 0x/0o/0b prefix. */
        size_t lead = 0;
        if (n > 0 && (src[0] == '-' || src[0] == '+')) lead = 1;
        kn_push_cstr(dst, sign);
        kn_string_push(dst, src, lead);
        if (n >= lead + 2 && src[lead] == '0' &&
            (src[lead + 1] == 'x' || src[lead + 1] == 'o' || src[lead + 1] == 'b')) {
            kn_string_push(dst, src + lead, 2);
            lead += 2;
        }
        for (int64_t i = 0; i < pad; i++) kn_push_cstr(dst, "0");
        kn_string_push(dst, src + lead, n - lead);
        return;
    }
    if (align == 0) align = numeric ? 3 : 1;
    int64_t before = align == 3 ? pad : align == 2 ? pad / 2 : 0;
    int64_t after = pad - before;
    for (int64_t i = 0; i < before; i++) kn_push_utf8(dst, fill);
    kn_push_cstr(dst, sign);
    kn_string_push(dst, src, n);
    for (int64_t i = 0; i < after; i++) kn_push_utf8(dst, fill);
}

/* `std::mem::black_box`: an opaque use of the value's memory. */
void kn_black_box(void *p) {
#ifndef _MSC_VER
	__asm__ volatile("" : : "r"(p) : "memory");
#else
	(void)p;
#endif
}
