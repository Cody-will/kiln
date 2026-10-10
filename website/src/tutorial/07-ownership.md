# Ownership and borrowing

This is the part of Kiln that comes from Rust. It is how Kiln is memory-safe with **no garbage collector and no manual freeing**.

<div class="status">
✅ <strong>The borrow checker is implemented.</strong> <code>kiln check</code> enforces everything in this chapter: moves, borrows, lifetimes, and drop order. (Programs can't run yet: code generation is the next milestone.)
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

The rules for leaving lifetimes out (*elision*): if a function takes `&self` or `&mut self`, a returned reference borrows from `self`; otherwise, if exactly one parameter is a reference, the result borrows from it. Anything else needs annotations. A reference that doesn't borrow from any parameter must be `'static`, like a string literal:

```kiln
fn greeting(formal: bool) -> &'static str {
	if formal {
		return "Good day"
	}
	return "hi"
}
```

`kiln check` enforces all of this. Returning a reference to a local is an error, because the local is dropped when the function returns:

```text
error: cannot return reference to local variable `s`
 --> f.kn:3:9
  |
3 | 	return &s
  | 	       ^^
  = note: returns a value referencing data owned by the current function, which is dropped when it returns
  = help: return an owned value instead (e.g. `String` rather than `&str`)
```

A struct that stores a reference declares a lifetime parameter, and the checker makes sure the struct doesn't outlive what it borrows:

```kiln
struct Parser<'src> {
	input: &'src str,
	pos: usize,
}

fn main() {
	text := String::from("let x = 1")
	p := Parser { input: text.as_str(), pos: 0 }
	println(p.input)
}
```

## What the borrow checker reports

Here is the error for using a value after moving it. The note points at the move:

```text
error: use of moved value: `s`
 --> moved.kn:6:2
  |
6 | 	take(s)
  | 	^^^^^^^
note: value moved here
 --> moved.kn:5:2
  |
5 | 	take(s)
  | 	^^^^^^^
  = note: move occurs because `s` has type `String`, which does not implement the `Copy` trait
  = help: borrow it with `&s` or call `.clone()` before moving it
```

Borrows end at their **last use**, not at the end of the block (*non-lexical lifetimes*), so this is fine:

```kiln
fn main() {
	v mut := Vec::new()
	v.push(1)
	first := &v[0]
	println("{first}")
	v.push(2) // `first` is no longer used, so `v` can be mutated
}
```

…but using `first` after `v.push(2)` would be an error, because `push` may reallocate the vector and leave `first` dangling.

Closures borrow what they use: shared if they only read a variable, mutable if they change it, and by value if they move it (or if declared `move`). Tasks started with `go` can outlive the function, so they must own what they use; see [Concurrency](09-concurrency.md).

## Drop and `defer`

Values are dropped in reverse order of declaration when their scope ends, so files and locks close themselves. For other cleanup, `defer` runs a call when the function returns:

```kiln
fn work() {
	defer println("done")
	println("working")
}
```

Deferred calls run after the return value is computed and before local variables are dropped, last-in first-out. They see variables as they are when the function returns, so moving a variable after deferring a call that uses it is an error.

When values with a `Drop` impl hold references, the borrow checker uses drop order: a value declared *before* the thing it borrows would be dropped *after* it, which is reported as "does not live long enough".

<div class="status">
🚧 <code>defer</code> and <code>Drop</code> are checked; they will run once code generation exists.
</div>
