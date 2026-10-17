# Calling C

Kiln programs can call C libraries directly. Three pieces are involved:

1. `extern "C"` declares the C functions you call.
2. `@repr(C)` makes a struct match C's layout.
3. The `[link]` section of `kiln.toml` says what to link: system libraries, search paths, and C files to compile.

## A first call

`hypot` is in the C math library, so declare it and link `libm`:

```kiln
extern "C" {
	fun hypot(x f64, y f64) f64
}

fun main() {
	h := unsafe { hypot(5.0, 12.0) }
	println("{h}") // 13
}
```

```toml
[package]
name = "ffi"
version = "0.1.0"

[link]
libs = ["m"]
```

Foreign functions are `unsafe`: the compiler can't check what C code does with memory, so calls go inside `unsafe { ... }`. Wrap them in a safe Kiln function once you've checked the contract.

## Sharing a struct with C

```c
// native/point.c
#include <stdint.h>
typedef struct { double x, y; int32_t tag; } Point;
void point_scale(Point *p, double k) { p->x *= k; p->y *= k; p->tag += 1; }
```

```kiln
@repr(C)
struct Point {
	x f64,
	y f64,
	tag i32,
}

extern "C" {
	fun point_scale(p *mut Point, k f64)
}

fun main() {
	p mut := Point { x: 3.0, y: 4.0, tag: 7 }
	unsafe { point_scale(&mut p as *mut Point, 2.0) }
	println("{p.x} {p.y} {p.tag}") // 6 8 8
}
```

```toml
[link]
c-sources = ["native/point.c"]
```

`kiln build` compiles `native/point.c` with the system C compiler and links it in.

## What may cross the boundary

| Kiln | C |
|---|---|
| `i8` … `i64`, `u8` … `u64`, `isize`, `usize` | `int8_t` … `uint64_t`, `intptr_t`, `size_t` |
| `f32`, `f64` | `float`, `double` |
| `bool` | `bool` (`_Bool`) |
| `*const T`, `*mut T` | `const T *`, `T *` |
| `void` result | `void` |

Strings, references, closures, and structs without `@repr(C)` are rejected, with a message saying what to use instead:

- for a string, pass `s.as_ptr()` and `s.len()`;
- for a reference, cast it with `r as *const T`.

Structs are passed by pointer, never by value.

## `[link]` reference

| Key | Meaning |
|---|---|
| `libs` | libraries to link (`-lname`; `name.lib` on Windows) |
| `search` | directories to search for them, relative to the package |
| `c-sources` | C files to compile and link (not yet on Windows) |

Dependencies' `[link]` sections are merged into the program's, so a package that wraps a C library brings its linking setup with it.
