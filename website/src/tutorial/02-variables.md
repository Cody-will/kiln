# Variables and types

## Declaring variables

Kiln has two ways to declare a variable. Both are immutable unless you say otherwise.

```kiln
fn main() {
	// Short declaration (like Go), for most code.
	count := 10
	name := "kiln"

	// `let`, when you want a type annotation.
	let ratio: f64 = 0.5
	let small: u8 = 255

	// Mutable variables.
	total mut := 0
	let mut steps = 0
	total += count
	steps += 1
	println("{name} {ratio} {small} {total} {steps}")
}
```

The rules:

- `x := v` declares a new variable. It never assigns to an existing one; to assign, use `x = v`.
- `x mut := v` and `let mut x = v` make the variable mutable.
- Type annotations go only on `let`: `let x: u64 = 1`.
- You can destructure tuples: `(a, b) := pair()`, `(q, r mut) := divmod(7, 2)`.
- Declaring a name again shadows the old one: `x := x + 1`.

Assigning to an immutable variable is a compile error:

```text
error: cannot assign twice to immutable variable `count`
  = help: make it mutable: `count mut := ...` or `let mut count = ...`
```

## Type inference

Kiln infers types inside a function. Integer literals default to `i32` and float literals to `f64`, unless context requires something else:

```kiln
fn takes_u64(n: u64) -> u64 {
	return n * 2
}

fn main() {
	n := 21          // inferred as u64 from the call below
	m := takes_u64(n)
	x := 1.5         // f64
	println("{m} {x}")
}
```

## Built-in types

| Kind | Types |
|---|---|
| Signed integers | `i8` `i16` `i32` `i64` `i128` `isize` |
| Unsigned integers | `u8` `u16` `u32` `u64` `u128` `usize` |
| Floats | `f32` `f64` |
| Other scalars | `bool`, `char` (a Unicode scalar value) |
| Text | `str` (borrowed as `&str`), `String` (owned, growable) |
| Compound | tuples `(i32, bool)`, arrays `[u8; 4]`, slices `&[T]` |
| Unit | `()`, the "no value" type |

Literals can carry a type suffix (`10u8`, `2.5f32`) and use `_` separators (`1_000_000`), hex (`0xff`), octal (`0o17`), and binary (`0b1010`).

## Conversions are explicit

Kiln never converts between numeric types implicitly. Use `as`:

```kiln
fn main() {
	let a: i32 = 7
	b := a as f64 / 2.0
	c := 'A' as u32
	println("{b} {c}")
}
```

Mixing types without a conversion is an error: `1 + 2.0` reports ``cannot apply `+` to `{integer}` and `{float}` ``.

## Constants

```kiln
const MAX_USERS: usize = 1000

fn main() {
	println("limit: {MAX_USERS}")
}
```
