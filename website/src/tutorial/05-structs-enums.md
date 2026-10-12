# Structs and enums

## Structs

```kiln
struct Point {
	x f64,
	y f64,
}

impl Point {
	// An associated function (no `self`), called as `Point::new(...)`.
	fun new(x f64, y f64) Point {
		return Point { x: x, y: y }
	}

	// A method that reads `self`.
	fun len(&self) f64 {
		return (self.x * self.x + self.y * self.y).sqrt()
	}

	// A method that modifies `self`.
	fun scale(&mut self, k f64) {
		self.x *= k
		self.y *= k
	}
}

fun main() {
	p mut := Point::new(3.0, 4.0)
	p.scale(2.0)
	println("length {p.len()}")
}
```

- Struct literals name every field: `Point { x: 1.0, y: 2.0 }`. Missing or unknown fields are errors.
- `&self` borrows the value, `&mut self` borrows it mutably, and `self` takes ownership.
- Calling a `&mut self` method needs a mutable variable (`p mut := ...`).
- Multi-line field lists end with a trailing comma (`kiln fmt` enforces this).

Tuple structs and unit structs:

```kiln
struct Meters(f64)
struct Marker

fun main() {
	m := Meters(5.0)
	println("{m.0} meters")
	flag := Marker
}
```

## Enums

An enum value is one of several variants, and each variant can carry data:

```kiln
enum Shape {
	Circle(f64),
	Rect { w f64, h f64 },
	Empty,
}

fun area(s &Shape) f64 {
	return match s {
		Shape::Circle(r) => 3.14159 * r * r,
		Shape::Rect { w, h } => w * h,
		Shape::Empty => 0.0,
	}
}

fun main() {
	shapes := [Shape::Circle(1.0), Shape::Rect { w: 2.0, h: 3.0 }, Shape::Empty]
	total mut := 0.0
	for s in &shapes {
		total += area(s)
	}
	println("total area {total}")
}
```

Matching on `&Shape` binds `r`, `w`, and `h` as references (`&f64`). Arithmetic works through references, so you rarely need `*`.

If you forget a variant, the compiler says which one:

```text
error: non-exhaustive match: `Empty` not covered
  = help: add arms for the missing cases or a `_ => ...` arm
```

## `option`

There is no `null`. A value that may be absent has type `option<T>`, which is either `Some(value)` or `None`:

```kiln
fun find(names &[&str], target &str) option<usize> {
	for i in 0..names.len() {
		if names[i] == target {
			return Some(i)
		}
	}
	return None
}

fun main() {
	names := ["ada", "grace"]
	match find(&names, "grace") {
		Some(i) => println("found at {i}"),
		None => println("not found"),
	}
}
```

<div class="status">
Exhaustiveness checking looks inside patterns: <code>Some(true)</code>, <code>Some(false)</code>, and <code>None</code> together cover an <code>Option&lt;bool&gt;</code>. 🚧 Integer ranges are not counted yet, so matches on integers need a <code>_</code> arm.
</div>

## Printing values

Interpolation takes Rust-style format specs after a colon. `:?` prints the
debug form, which `@derive(Debug)` builds from the type's structure:

```kiln
@derive(Debug)
struct Point {
    x i32,
    y f64,
}

fun main() {
    p := Point { x: 1, y: 2.5 }
    println("{p:?}")        // Point { x: 1, y: 2.5 }
    println("{p.y:>8.2}|")  //     2.50|
    println("{255:#x}")     // 0xff
}
```

`{p:#?}` pretty-prints over several lines. Width, fill, alignment (`<` `^` `>`),
`+`, zero padding, precision, and `x` `o` `b` `e` all work as in Rust.
