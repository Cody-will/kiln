# Ownership and borrowing

This is the part of Kiln that comes from Rust. It is how Kiln is memory-safe with **no garbage collector and no manual freeing**.

<div class="status">
🚧 <strong>The borrow checker is not implemented yet.</strong> <code>kiln check</code> already enforces mutability (<code>mut</code> bindings, <code>&amp;mut</code> borrows, <code>&amp;mut self</code> calls), but it does not yet report use-after-move, conflicting borrows, or dangling references. The rules below are the language rules; the compiler will enforce them in milestone M3.
</div>

## The three rules

1. Every value has exactly **one owner**.
2. When the owner goes out of scope, the value is **dropped** (freed).
3. You can have **either** any number of shared borrows `&T` **or** one mutable borrow `&mut T` at a time, and a borrow can't outlive the value.

## Moves

Assigning a value or passing it to a function **moves** it, unless its type is `Copy` (integers, floats, `bool`, `char`, shared references, and tuples/arrays of those).

```kiln
fn consume(s: String) {
	println(s)
}

fn main() {
	s := String::from("hello")
	consume(s)        // ownership moves into `consume`
	// println(s)     // error (M3): use of moved value `s`

	n := 5
	m := n            // i32 is Copy: both are usable
	println("{n} {m}")

	a := String::from("x")
	b := a.clone()    // an explicit deep copy
	println("{a} {b}")
}
```

## Borrowing

To use a value without taking it, borrow it:

```kiln
fn length(s: &String) -> usize {
	return s.len()
}

fn shout(s: &mut String) {
	s.push("!")
}

fn main() {
	s mut := String::from("hi")
	n := length(&s)    // shared borrow
	shout(&mut s)      // mutable borrow: `s` must be declared `mut`
	println("{s} ({n})")
}
```

Taking `&mut` of an immutable variable is already an error today:

```text
error: cannot borrow `s`, as it is not declared as mutable
  = help: make it mutable: `s mut := ...` or `let mut s = ...`
```

### `&str` and `&String`

String literals have type `&str` (borrowed text). A `&String` converts to `&str` automatically, so functions should usually take `&str`:

```kiln
fn greet(name: &str) {
	println("hi {name}")
}

fn main() {
	owned := String::from("ada")
	greet(&owned)    // &String → &str
	greet("grace")
}
```

The same applies to `&Vec<T>` → `&[T]` and `&Box<T>` → `&T`.

## Lifetimes

A lifetime names how long a borrow is valid. Kiln infers lifetimes when they are obvious and asks for them when they aren't:

```kiln
// One input reference: the output borrows from it (inferred).
fn trimmed(s: &str) -> &str {
	return s.trim()
}

// Two inputs: say which one the result borrows from.
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
	if a.len() > b.len() {
		return a
	}
	return b
}
```

<div class="status">
🚧 Lifetimes are parsed and accepted today but not checked.
</div>

## Drop and `defer`

Values are dropped in reverse order of declaration when their scope ends, so files and locks close themselves. For other cleanup, `defer` runs a call when the function returns:

```kiln
fn work() {
	defer println("done")
	println("working")
}
```

<div class="status">
🚧 <code>defer</code> is type-checked; it will run once code generation exists.
</div>
