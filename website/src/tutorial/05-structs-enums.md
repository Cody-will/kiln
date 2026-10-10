# Structs and enums

## Structs

```kiln
struct Point {
	x: f64,
	y: f64,
}

impl Point {
	// An associated function (no `self`), called as `Point::new(...)`.
	fn new(x: f64, y: f64) -> Point {
		return Point { x: x, y: y }
	}

	// A method that reads `self`.
	fn len(&self) -> f64 {
		return (self.x * self.x + self.y * self.y).sqrt()
	}

	// A method that modifies `self`.
	fn scale(&mut self, k: f64) {
		self.x *= k
		self.y *= k
	}
}

fn main() {
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

fn main() {
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
	Rect { w: f64, h: f64 },
	Empty,
}

fn area(s: &Shape) -> f64 {
	return match s {
		Shape::Circle(r) => 3.14159 * r * r,
		Shape::Rect { w, h } => w * h,
		Shape::Empty => 0.0,
	}
}

fn main() {
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

## `Option`

There is no `null`. A value that may be absent has type `Option<T>`, which is either `Some(value)` or `None`:

```kiln
fn find(names: &[&str], target: &str) -> Option<usize> {
	for i in 0..names.len() {
		if names[i] == target {
			return Some(i)
		}
	}
	return None
}

fn main() {
	names := ["ada", "grace"]
	match find(&names, "grace") {
		Some(i) => println("found at {i}"),
		None => println("not found"),
	}
}
```

<div class="status">
🚧 Exhaustiveness checking currently looks at the top-level variant only. Nested patterns like <code>Some(Shape::Empty)</code> count as partial coverage, and integer ranges need a <code>_</code> arm.
</div>
