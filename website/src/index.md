<div class="hero">
<h1>Kiln</h1>
<p class="tagline">Syntax as simple as Go. Functionality like Rust.</p>
</div>

Kiln is a compiled programming language for **systems programming and backend services**. It pairs Rust's memory model (ownership, borrowing, no garbage collector, no manual `free`) with Go's small syntax and its goroutine-and-channel concurrency.

```kiln
fn main() {
	(tx, rx) := chan::<String>(0)
	go greet(tx)
	msg := <-rx
	println(msg)
}

fn greet(out: chan<- String) {
	out <- String::from("Hello from a task!")
}
```

<div class="status">
<strong>Kiln is early and not usable for real programs yet.</strong> Today the compiler type-checks and borrow-checks Kiln programs (<code>kiln check</code>) and compiles single-file programs to native executables (<code>kiln build</code>, <code>kiln run</code>). Tasks, channels, and <code>select</code> run on a work-stealing scheduler. The standard library is still to come. See <a href="status.html">Implementation status</a>. Throughout this book, 🚧 marks features that are specified but not implemented yet.
</div>

## Goals

<div class="features">
<div>

### Memory-safe without a GC

Every value has one owner. References are checked borrows (`&T`, `&mut T`) with lifetimes inferred when they are obvious. Memory is freed deterministically when its owner goes out of scope.

</div>
<div>

### Simple, Go-like syntax

Newlines end statements, `x := v` declares a variable, there is one canonical formatter (`kiln fmt`), and the keyword set is small.

</div>
<div>

### Go-style concurrency

`go f()` starts a lightweight task. Typed channels (`chan<- T`, `<-chan T`) and `select` coordinate them. The ownership rules mean data races are compile errors.

</div>
<div>

### Errors as values

`Result<T, E>` and `?` let an error bubble up to the caller that can handle it, with no `if err != nil` after every call and no exceptions.

</div>
<div>

### Generics and traits

Rust-style traits, generics with bounds, and `#[derive]`. Generics compile to specialized code (monomorphization), so they have no runtime cost.

</div>
<div>

### Explicit by design

Functions `return` explicitly, mutation is opt-in (`x mut :=`), and conversions use `as` or `From`. The language leaves little to guess.

</div>
</div>

## Where to go next

- [Install Kiln from source](install.md)
- [Start the tutorial](tutorial/01-hello.md)
- [Read the language specification](spec.md)
