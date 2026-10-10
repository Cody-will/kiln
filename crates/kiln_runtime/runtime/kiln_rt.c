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

KN_NORETURN void kn_panic(const uint8_t *msg, size_t len) {
	fflush(stdout);
	fputs("panic: ", stderr);
	fwrite(msg, 1, len, stderr);
	fputc('\n', stderr);
	fflush(stderr);
	exit(101);
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
	fwrite(p, 1, len, f);
	if (newline)
		fputc('\n', f);
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

void kn_fmt_i64(KnVec *s, int64_t v) {
	char buf[32];
	int n = snprintf(buf, sizeof buf, "%lld", (long long)v);
	kn_string_push(s, (const uint8_t *)buf, (size_t)n);
}

void kn_fmt_u64(KnVec *s, uint64_t v) {
	char buf[32];
	int n = snprintf(buf, sizeof buf, "%llu", (unsigned long long)v);
	kn_string_push(s, (const uint8_t *)buf, (size_t)n);
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
	char buf[64];
	int prec;
	for (prec = 1; prec <= 17; prec++) {
		snprintf(buf, sizeof buf, "%.*e", prec - 1, v);
		if (strtod(buf, NULL) == v)
			break;
	}
	/* buf = [-]d.ddde[+-]XX */
	char digits[32];
	int nd = 0, neg = 0, i = 0;
	if (buf[0] == '-') {
		neg = 1;
		i = 1;
	}
	for (; buf[i] && buf[i] != 'e'; i++)
		if (buf[i] >= '0' && buf[i] <= '9')
			digits[nd++] = buf[i];
	int exp = atoi(buf + i + 1);
	while (nd > 1 && digits[nd - 1] == '0')
		nd--;
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
