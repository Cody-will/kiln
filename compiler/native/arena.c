/* Fast allocator for the kilnc binary only (linked via kiln.toml [link]).
 *
 * The compiler is a short-lived batch process that allocates and frees
 * millions of small objects. glibc malloc spends ~25% of front-end time in
 * malloc/free. This replaces the malloc family with per-thread size-class
 * free lists carved from bump-allocated 4 MiB chunks; memory is never
 * returned to the OS (the process exits soon). Blocks larger than
 * ARENA_MAX go to glibc. Every block has a 16-byte header recording its
 * class so free/realloc need no size. */
#define _GNU_SOURCE
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>

extern void *__libc_malloc(size_t);
extern void __libc_free(void *);
extern void *__libc_realloc(void *, size_t);
extern void *__libc_memalign(size_t, size_t);

#define HDR 16
#define STEP 16
#define ARENA_MAX 2048
#define NCLASS (ARENA_MAX / STEP)
#define CHUNK (4u << 20)
#define LARGE 0xffffu
#define ALIGNED 0xfffeu

typedef struct { uint32_t cls; uint32_t magic; void *base; } hdr_t;
typedef struct node { struct node *next; } node_t;

static __thread node_t *freelist[NCLASS + 1];
static __thread char *bump, *bump_end;

static void *refill(size_t n) {
	if ((size_t)(bump_end - bump) < n) {
		void *c = mmap(0, CHUNK, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (c == MAP_FAILED)
			return 0;
		bump = c;
		bump_end = (char *)c + CHUNK;
	}
	void *p = bump;
	bump += n;
	return p;
}

static inline hdr_t *H(void *p) { return (hdr_t *)((char *)p - HDR); }

void *malloc(size_t n) {
	if (n > ARENA_MAX) {
		hdr_t *h = __libc_malloc(n + HDR);
		if (!h)
			return 0;
		h->cls = LARGE;
		return (char *)h + HDR;
	}
	size_t c = (n + STEP - 1) / STEP;
	if (c == 0)
		c = 1;
	node_t *f = freelist[c];
	if (f) {
		freelist[c] = f->next;
		return f;
	}
	hdr_t *h = refill(c * STEP + HDR);
	if (!h) {
		errno = ENOMEM;
		return 0;
	}
	h->cls = (uint32_t)c;
	return (char *)h + HDR;
}

void free(void *p) {
	if (!p)
		return;
	hdr_t *h = H(p);
	if (h->cls == LARGE) {
		__libc_free(h);
	} else if (h->cls == ALIGNED) {
		__libc_free(h->base);
	} else {
		node_t *f = p;
		f->next = freelist[h->cls];
		freelist[h->cls] = f;
	}
}

static size_t usable(void *p) {
	hdr_t *h = H(p);
	if (h->cls == LARGE || h->cls == ALIGNED)
		return 0; /* unknown: realloc copies conservatively below */
	return h->cls * STEP;
}

void *realloc(void *p, size_t n) {
	if (!p)
		return malloc(n);
	if (n == 0) {
		free(p);
		return 0;
	}
	hdr_t *h = H(p);
	if (h->cls == LARGE && n > ARENA_MAX) {
		hdr_t *q = __libc_realloc(h, n + HDR);
		return q ? (char *)q + HDR : 0;
	}
	size_t have = usable(p);
	if (h->cls != LARGE && h->cls != ALIGNED && n <= have)
		return p;
	void *q = malloc(n);
	if (!q)
		return 0;
	if (h->cls == LARGE) {
		/* glibc knows the real size; n < old size here, so copy n */
		memcpy(q, p, n);
	} else if (h->cls == ALIGNED) {
		memcpy(q, p, h->magic < n ? h->magic : n);
	} else {
		memcpy(q, p, have < n ? have : n);
	}
	free(p);
	return q;
}

void *calloc(size_t a, size_t b) {
	size_t n;
	if (__builtin_mul_overflow(a, b, &n)) {
		errno = ENOMEM;
		return 0;
	}
	void *p = malloc(n);
	if (p)
		memset(p, 0, n);
	return p;
}

void *memalign(size_t al, size_t n) {
	if (al <= HDR)
		return malloc(n);
	if (n > 0xffffffffu)
		return 0;
	char *base = __libc_malloc(n + al + HDR);
	if (!base)
		return 0;
	uintptr_t u = ((uintptr_t)base + HDR + al - 1) & ~(uintptr_t)(al - 1);
	hdr_t *h = H((void *)u);
	h->cls = ALIGNED;
	h->magic = (uint32_t)n; /* size, for realloc */
	h->base = base;
	return (void *)u;
}
int posix_memalign(void **out, size_t al, size_t n) {
	void *p = memalign(al, n);
	if (!p)
		return ENOMEM;
	*out = p;
	return 0;
}
void *aligned_alloc(size_t al, size_t n) { return memalign(al, n); }
void *valloc(size_t n) { return memalign(4096, n); }
void *pvalloc(size_t n) { return memalign(4096, (n + 4095) & ~(size_t)4095); }
size_t malloc_usable_size(void *p) { return p ? usable(p) : 0; }
