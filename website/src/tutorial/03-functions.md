# Functions

```kiln
fun add(a i32, b i32) i32 {
	return a + b
}

fun greet(name &str) {
	println("hi, {name}")
}

fun main() {
	greet("ada")
	println("{add(2, 3)}")
}
```

- Parameters are written `name Type` (no colon), and the return type follows the parameter list (no `->`).
- A function without a return type returns `void` (nothing).
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
fun divmod(a u32, b u32) (u32, u32) {
	return (a / b, a % b)
}

fun main() {
	(q, r) := divmod(17, 5)
	println("{q} remainder {r}")
}
```

## Closures

Closures are anonymous functions written `|params| body`. Parameter types are usually inferred:

```kiln
fun apply(f fun(i32) i32, x i32) i32 {
	return f(x)
}

fun main() {
	double := |x| x * 2
	println("{apply(double, 21)}")

	// A closure with a block body uses `return`, just like a function.
	clamp := |x i32| {
		if x > 100 {
			return 100
		}
		return x
	}
	println("{clamp(250)}")
}
```

A single-expression closure (`|x| x * 2`) returns its expression; a closure with a `{ }` body follows the function rule.

A function value of type `fun(..)` may be copied and shared, even between tasks, so a closure used as one may only *read* what it captures. A callback that changes captured variables has the type `fun mut(..)` instead:

```kiln
fun each(xs &[i32], f fun mut(i32)) {
	for x in xs {
		f(*x)
	}
}

fun main() {
	total mut := 0
	each(&[1, 2, 3], |x| {
		total += x
	})
	println("{total}")
}
```

Any `fun` can be passed where a `fun mut` is expected. A closure that moves a captured value out (`move || drop(s)`) can only be called directly, since a second call would find the value gone.

