# Functions

```kiln
fn add(a: i32, b: i32) -> i32 {
	return a + b
}

fn greet(name: &str) {
	println("hi, {name}")
}

fn main() {
	greet("ada")
	println("{add(2, 3)}")
}
```

- Parameters always have types, and the return type follows `->`.
- A function without `->` returns `()` (nothing).
- Function names use `snake_case`, and types use `PascalCase`.

## Returns are explicit

Kiln functions **always** use `return` to produce a value. Unlike Rust, the last expression in a function body is *not* returned implicitly:

```text
fn double(x: i32) -> i32 {
	x * 2
}
```

```text
error: missing `return`: Kiln functions do not return their last expression implicitly
 --> double.kn:2:2
  |
2 | 	x * 2
  | 	^^^^^
  = help: write `return <expr>`
```

The compiler also checks that **every path** returns:

```text
fn sign(x: i32) -> i32 {
	if x < 0 {
		return -1
	}
}
// error: function `sign` must return a value of type `i32` on every path
```

A path that ends in `panic(...)`, an infinite `loop`, or a `match` where every arm returns counts as returning.

## Multiple results

Return a tuple and destructure it with `:=`:

```kiln
fn divmod(a: u32, b: u32) -> (u32, u32) {
	return (a / b, a % b)
}

fn main() {
	(q, r) := divmod(17, 5)
	println("{q} remainder {r}")
}
```

## Closures

Closures are anonymous functions written `|params| body`. Parameter types are usually inferred:

```kiln
fn apply(f: fn(i32) -> i32, x: i32) -> i32 {
	return f(x)
}

fn main() {
	double := |x| x * 2
	println("{apply(double, 21)}")

	// A closure with a block body uses `return`, just like a function.
	clamp := |x: i32| {
		if x > 100 {
			return 100
		}
		return x
	}
	println("{clamp(250)}")
}
```

A single-expression closure (`|x| x * 2`) returns its expression; a closure with a `{ }` body follows the function rule.

<div class="status">
🚧 Closure captures are type-checked but not yet analysed for borrowing. <code>move</code> closures parse but behave like ordinary ones until the borrow checker lands.
</div>
