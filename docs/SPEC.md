# Kiln Language Specification — Draft 0.1

Kiln is a compiled, statically typed language for systems programming and backend services. It combines Go's readability and built-in concurrency with Rust's ownership, borrowing, traits, and `Result`-based errors. There is no garbage collector and no manual `free`.

## 1. Design Goals

**Guiding principle: syntax as simple as Go, functionality like Rust.** When the two conflict, prefer the simpler surface syntax, provided it gives up no safety or expressiveness.

1. **Memory safety without GC**: ownership plus borrow checking at compile time, with deterministic destruction (RAII).
2. **Readable by default**: Go-like surface syntax, few sigils, explicit `return`, `:=` short declarations, and one canonical formatting style (`kiln fmt`, no options).
3. **Expressive types**: generics, traits, sum types (enums), and pattern matching.
4. **Cheap concurrency**: `go` spawns lightweight tasks; channels and `select` are part of the language. Data races are compile errors.
5. **Explicit errors**: `Result<T, E>` plus `?`. No exceptions and no `if err != nil`.
6. **Predictable performance**: zero-cost abstractions, monomorphized generics, an LLVM backend, and C ABI interop.
7. **First-class tooling**: compiler, formatter, package manager, language server, and highlighting ship together.

Non-goals for 1.0: inheritance, implicit numeric conversions, macros that rewrite syntax arbitrarily, and async/await coloring.

## 2. Lexical Structure

- Source files are UTF-8 with the `.kn` extension (decided).
- Comments are `//` for a line, `/* */` for a block (nestable), and `///` for doc comments.
- Identifiers match `[A-Za-z_][A-Za-z0-9_]*`. Visibility is set with `pub`, not by capitalization (unlike Go).
- **Semicolons**: newlines terminate statements, using Go-style automatic insertion.
  - A `;` is inserted at a newline when the line ends with an identifier, a literal, a lifetime, `break`, `continue`, `return`, `true`, `false`, `self`, `Self`, `)`, `]`, `}`, `?`, or `_`. A line ending in anything else (an operator, `,`, `(`, `{`, `.`) continues.
  - A line that begins with `.` (but not `..`) continues the previous line, so method chains can be split across lines.
  - Inside `(...)`, `[...]`, `<...>`, and `{...}` lists, newlines between elements are ignored, so a missing trailing comma before a newline still parses. `kiln fmt` adds the comma.
  - Statement-level consequence: `} else {` and a `where` clause must stay on the same line as what precedes them, as in Go.
- **Literals**:
  - Integers: `42`, `0xFF`, `0b1010`, `1_000`, with optional suffixes `42u8` and `3i64`.
  - Floats: `3.14` and `1e9f32`.
  - Strings: `"..."`, raw strings `r"..."`, and interpolation `"hi {name}"`.
  - Characters: `'a'`.
  - Booleans: `true` and `false`.

### Keywords

```
as break chan const continue defer else enum false fn for go if impl
import in let loop match mod move mut pub return select self Self
static struct trait true type unsafe use where while
```

Reserved for future use: `async await macro yield try`.

### Operators

Kiln uses Rust's operator set: `+ - * / % & | ^ << >> && || ! == != < <= > >= = += -= ...`. On top of that:

| Operator | Meaning |
|---|---|
| `&` / `&mut` | Borrow |
| `?` | Propagate an error |
| `<-` | Channel send and receive |
| `..` / `..=` | Ranges |
| `::` | Path separator |
| `:=` | Short declaration (`x := v`, `x mut := v`) |
| `=>` | Match arm |

## 3. Statements, Declarations, and Returns

### 3.1 Explicit `return` (decided)

- A function body never returns its last expression. A function with a return type must `return` on every path; the compiler checks this ("missing return", as in Go).
- A function with no `-> T` returns `()` (unit). A bare `return` or falling off the end is allowed there.
- `main` is `fn main()` or `fn main() -> Result<(), E>`.

### 3.2 Value-producing `if` / `match` (decided)

`if` and `match` stay expressions, so they can produce a value, but only in these places:

- **Value position**: the right-hand side of `let` or `:=`, a `return` operand, a function argument, a struct field, or an assignment right-hand side.
- **Selected value**: when an `if` or `match` is in value position, each branch's final expression is the value. An `if` used as a value must have an `else`.
- **Match arms**: `pat => expr` is a single expression. A block arm `pat => { ...; expr }` ends with its value expression.
- **Statements**: an `if` or `match` used as a statement produces `()`.
- **No plain value blocks**: a bare `{ ... }` block cannot produce a value. Functions and loops never yield their last expression.
- **Closures**:
  - The single-expression form `|x| x + 1` yields its expression.
  - A block-bodied closure `|x| { ... }` needs `return`, exactly like a function.

```kiln
fn sign(n: i32) -> &str {
	label := if n < 0 { "neg" } else if n == 0 { "zero" } else { "pos" }
	return label
}

fn area(s: &Shape) -> f64 {
	return match s {
		Shape::Circle(r) => 3.14159 * r * r,
		Shape::Rect { w, h } => w * h,
	}
}
```

This keeps "where does this function return?" obvious, as in Go, while keeping Rust's expressive `match`.

### 3.3 Variable declarations (decided)

```
LetDecl   = "let" ["mut"] Pattern [":" Type] "=" Expr
ShortDecl = ShortLhs ":=" Expr
ShortLhs  = Ident ["mut"] | "(" ShortItem { "," ShortItem } ")"
ShortItem = Ident ["mut"] | "_"
Assign    = Place AssignOp Expr        // =, +=, -=, ...
```

Rules:

1. `x := v` is exactly `let x = v`, and `x mut := v` is exactly `let mut x = v`.
2. `:=` always declares new bindings, and shadowing an outer or earlier `x` is allowed (as with `let`). Go's partial-redeclare behavior does not exist: `:=` never assigns to an existing variable. To mutate, use `=`.
3. Type annotations are only allowed with `let` (`let x: u8 = 5`). To give a type with `:=`, use a typed literal or a cast (`x := 5u8`).
4. With `:=`, the left side can be a single name or a flat tuple of names (`(a, b mut) := pair()`). Full patterns (`Some(x)`, struct destructuring) need `let` or `if let`.
5. Both forms work only in function bodies. Module-level items use `const` and `static`.
6. Style: `kiln fmt` keeps whichever form you wrote. `:=` is idiomatic for simple locals, and `let` for annotated or destructured bindings.

## 4. Examples

All examples are in canonical `kiln fmt` style (§10).

### Hello world

```kiln
fn main() {
	println("Hello, Kiln!")
}
```

### Variables and functions

```kiln
fn add(a: i32, b: i32) -> i32 {
	return a + b
}

fn main() {
	x := 5
	total mut := 0
	let limit: i64 = 10
	for i in 0..limit {
		total += add(i as i32, x)
	}
	println("total = {total}")
}
```

### Structs and methods

```kiln
pub struct Point {
	pub x: f64,
	pub y: f64,
}

impl Point {
	pub fn new(x: f64, y: f64) -> Point {
		return Point { x, y }
	}

	pub fn len(&self) -> f64 {
		return (self.x * self.x + self.y * self.y).sqrt()
	}

	pub fn scale(&mut self, k: f64) {
		self.x *= k
		self.y *= k
	}
}
```

### Enums and match

```kiln
enum Shape {
	Circle(f64),
	Rect { w: f64, h: f64 },
}

fn describe(s: &Shape) {
	match s {
		Shape::Circle(r) => println("circle r={r}"),
		Shape::Rect { w, h } => println("rect {w}x{h}"),
	}
}
```

### Traits and generics

```kiln
trait Describe {
	fn describe(&self) -> String

	fn shout(&self) -> String {
		return self.describe().to_upper()
	}
}

impl Describe for Point {
	fn describe(&self) -> String {
		return "({self.x}, {self.y})"
	}
}

fn print_all<T: Describe>(items: &[T]) {
	for it in items {
		println(it.describe())
	}
}

fn largest<T>(xs: &[T]) -> Option<&T> where T: Ord {
	best mut := None
	for x in xs {
		if best.is_none() || x > best.unwrap() {
			best = Some(x)
		}
	}
	return best
}

fn show(d: &dyn Describe) {
	println(d.describe())
}
```

### Ownership and borrowing

```kiln
fn consume(s: String) {
	println(s)
}

fn peek(s: &String) -> usize {
	return s.len()
}

fn push(s: &mut String) {
	s.push("!")
}

fn main() {
	s mut := String::from("hi")
	peek(&s)
	push(&mut s)
	consume(s) // s moves here
	// println(s)  // error: use of moved value `s`

	a := String::from("x")
	b := a.clone() // explicit deep copy; `Copy` types copy implicitly
}
```

The rules are Rust's:

- Every value has exactly one owner.
- At any time a value can have either many `&T` borrows or one `&mut T` borrow, never both.
- A borrow cannot outlive the value it points to.
- When the owner goes out of scope, the value is dropped (`Drop` trait), in reverse declaration order.

### Lifetimes

Lifetimes are inferred under Rust-style elision rules:

- A single reference input gives its lifetime to the output.
- In a method taking `&self`, the output takes `self`'s lifetime.

When elision is ambiguous, the compiler requires explicit lifetimes:

```kiln
fn first_word(s: &str) -> &str {
	return s.split(' ').next().unwrap_or("")
}

fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
	if a.len() > b.len() {
		return a
	}
	return b
}

struct Parser<'src> {
	input: &'src str,
	pos: usize,
}
```

### Errors

```kiln
enum ConfigError {
	Io(io::Error),
	Parse { line: u32, msg: String },
}

impl From<io::Error> for ConfigError {
	fn from(e: io::Error) -> Self {
		return ConfigError::Io(e)
	}
}

fn load(path: &str) -> Result<Config, ConfigError> {
	text := fs::read_to_string(path)? // io::Error converted via From
	cfg := parse(&text)?
	return Ok(cfg)
}

fn main() {
	match load("app.toml") {
		Ok(cfg) => run(cfg),
		Err(ConfigError::Io(e)) => eprintln("io: {e}"),
		Err(ConfigError::Parse { line, msg }) => eprintln("line {line}: {msg}"),
	}
}
```

`main` may return `Result<(), E>` where `E: Error`. If it returns `Err`, the program prints the error and exits with code 1. `panic` exists only for unrecoverable bugs.

### Concurrency: `go`, channels, `select`

```kiln
fn worker(id: u32, jobs: <-chan Job, results: chan<- Output) {
	for job in jobs { // loop ends when the channel is closed
		results <- process(job)
	}
}

fn main() {
	(jobs_tx, jobs_rx) := chan::<Job>(100)
	(res_tx, res_rx) := chan::<Output>(100)
	for id in 0..4 {
		go worker(id, jobs_rx.clone(), res_tx.clone())
	}
	for j in load_jobs() {
		jobs_tx <- j
	}
	drop(jobs_tx) // dropping the last sender closes the channel

	timeout := time::after(5.secs())
	loop {
		select {
			r := <-res_rx => println("got {r}"),
			<-timeout => {
				eprintln("timed out")
				break
			},
		}
	}
}
```

- `go f(args)` and `go move { ... }` spawn a task.
  - Captured values must be `Send + 'static`.
  - The task gets a value either by moving it in or by sharing it through `Arc<T>`.
- `go` returns a `Task<T>` handle. `task.join()` returns `Result<T, Panic>`, and dropping the handle detaches the task.
- `scope |s| { ... s.go(...) ... }` creates **scoped tasks**. These may borrow non-`'static` data and are joined before the scope returns.
- Types sent over a channel must be `Send`. Shared mutable state uses `Mutex<T>`, `RwLock<T>`, and atomics. `&T` crosses tasks only when `T: Sync`.
- **Channel types (decided, Go's exact forms)**:
  - `chan<- T` is a send-only end, and `<-chan T` is a receive-only end.
  - `chan::<T>(n)` returns `(chan<- T, <-chan T)`; `n = 0` is unbuffered.
  - Both ends are `Clone`, so there can be many producers and many consumers.
  - `chan T` (bidirectional) is not a separate type in Kiln: under ownership, each end is a separate value, so the direction is always explicit.
- `select` arms use `name := <-rx`, which is consistent with short declarations.

**Channel syntax (decided)**: channels use Go-style operators:

- Send with `tx <- v`.
- Receive with `v := <-rx`, which blocks until a value arrives. A receive on a closed channel ends a `for` loop and makes `<-rx` in a `select` arm skip.
- `for v in rx` receives until the channel is closed.

Non-blocking and explicit operations are library methods:

| Method | Returns |
|---|---|
| `tx.try_send(v)` | `Result<(), TrySendError<T>>` |
| `rx.try_recv()` | `Result<T, TryRecvError>` |
| `rx.recv()` | `Option<T>`; `None` once closed |
| `tx.close()` | closes the channel explicitly (dropping all senders also closes it) |
| `rx.len()` / `rx.cap()` | queued item count / buffer capacity |

As an expression statement, `<-rx` in non-`select` code receives and discards the value. Sending on a closed channel panics, as in Go; use `try_send` to avoid that.

### A small HTTP server sketch

```kiln
import std::net::{TcpListener, TcpStream}
import std::io::{self, BufReader}

fn handle(conn: TcpStream) -> Result<(), io::Error> {
	reader mut := BufReader::new(&conn)
	line := reader.read_line()?
	path := line.split(' ').nth(1).unwrap_or("/")
	body := match path {
		"/" => "Welcome to Kiln",
		"/health" => "ok",
		_ => {
			return respond(&conn, 404, "not found")
		},
	}
	return respond(&conn, 200, body)
}

fn respond(conn: &TcpStream, code: u16, body: &str) -> Result<(), io::Error> {
	conn.write_all("HTTP/1.1 {code} OK\r\nContent-Length: {body.len()}\r\n\r\n{body}")?
	return Ok(())
}

fn main() -> Result<(), io::Error> {
	listener := TcpListener::bind("0.0.0.0:8080")?
	println("listening on :8080")
	for conn in listener.incoming() {
		conn := conn?
		go move {
			if let Err(e) = handle(conn) {
				eprintln("conn error: {e}")
			}
		}
	}
	return Ok(())
}
```

Network I/O in a task never blocks an OS thread. The runtime parks the task on its netpoller (epoll, kqueue, or IOCP), the way Go does.

## 5. Type System

- **Primitives**:
  - Integers: `i8`–`i128`, `u8`–`u128`, `isize`, `usize`.
  - Floats: `f32` and `f64`.
  - Other: `bool`, `char` (a Unicode scalar value), `str` (UTF-8, unsized), and `()`.
- **Compound types**: tuples `(A, B)`, arrays `[T; N]`, slices `[T]`, references `&T` and `&mut T`, raw pointers `*const T` and `*mut T` (deref is `unsafe` only), and function types `fn(A) -> B`.
- **Closures** implement `Fn`, `FnMut`, or `FnOnce`, and may need `move`.
- **Nominal user types** are `struct`, `enum`, and `type Alias = ...`.
- **Generics** are monomorphized:
  - Bounds go inline or in a `where` clause.
  - Traits can have associated types and constants.
  - Const generics are planned for 1.x (`[T; N]` works in 1.0).
- **Traits**:
  - Implementations are explicit (`impl Trait for Type`), as in Rust, not structural like Go.
  - `dyn Trait` gives dynamic dispatch.
  - Coherence uses Rust's orphan rule.
- **Built-in marker traits**: `Copy`, `Send`, `Sync`, `Sized`. `Clone`, `Debug`, `Eq`, `Ord`, and `Hash` can be derived with `#[derive(...)]`.
- **Inference**: local Hindley–Milner-style inference within function bodies. Function signatures are always fully annotated.
- **No null**: `Option<T>` is used instead, and `Option<&T>` is pointer-sized.
- **Integer overflow**: panics in debug builds and wraps in release builds. `wrapping_*`, `checked_*`, and `saturating_*` are available.

## 6. Memory Model

- **Allocation**:
  - Values live on the stack by default.
  - Heap types are `Box<T>`, `Vec<T>`, `String`, `Rc<T>` (single task), and `Arc<T>` (shared across tasks).
- **Destruction** is deterministic: `Drop` runs at the end of the owner's scope. `defer` runs a block at scope exit, which is useful for non-RAII cleanup and FFI.
- **Borrow checking**:
  - Uses non-lexical lifetimes computed over the MIR control-flow graph.
  - The design targets a Polonius-style analysis but starts with NLL.
- **Interior mutability**: `Cell`, `RefCell`, `Mutex`, `RwLock`, and `Atomic*`.
- **Concurrency memory model**: C++11/Rust atomics orderings. Safe code cannot cause data races.
- **`unsafe` blocks** permit:
  - Dereferencing raw pointers.
  - Calling `unsafe fn` and FFI functions.
  - Accessing `static mut`.
  - Implementing `unsafe trait`s (`Send` and `Sync`).
- **FFI**: `extern "C" { fn ... }` and `#[export] extern "C" fn`.
- **Allocator**: the global allocator is pluggable (`#[global_allocator]`). Without the runtime, `no_std` mode supports embedded and kernel targets (see §11).

## 7. Modules, Packages, Build Tool

- Each file is a module, and each directory containing `mod.kn` is a module. A package is a directory with `kiln.toml`.
- `import std::fs` brings in a path, and `import foo::{A, B as C}` imports specific items. Items are private unless marked `pub`. `pub(pkg)` limits visibility to the package.
- An example manifest:

```toml
[package]
name = "webapp"
version = "0.1.0"
edition = "2027"

[dependencies]
http = "1.2"
json = { git = "https://github.com/kiln-lang/json", tag = "v0.3" }
```

- **The `kiln` CLI**:
  - Commands: `kiln new`, `build`, `run`, `test`, `bench`, `fmt`, `lint`, `doc`, `add`, and `publish`.
  - Lockfile: `kiln.lock`.
  - Registry: `pkg.kiln-lang.dev`, using semver resolution.
  - Builds are incremental per package.
- **Tests and benchmarks**: `#[test] fn ...` lives inline, and the `tests/` and `benches/` directories are recognized automatically.

## 8. Standard Library Outline

| Module | Contents |
|---|---|
| `core` | primitives, `Option`, `Result`, core traits, iterators, `mem`, `ptr` (no allocation, `no_std`-safe) |
| `alloc` | `Box`, `Vec`, `String`, `Rc`, `Arc`, `HashMap`, `BTreeMap`, `HashSet`, `VecDeque` |
| `std::io` | `Read`/`Write`, buffered I/O, stdin/stdout |
| `std::fs` | files, directories, paths |
| `std::net` | TCP, UDP, DNS, sockets integrated with the netpoller |
| `std::http` | HTTP/1.1 client and server (HTTP/2 planned), routing basics |
| `std::sync` | `Mutex`, `RwLock`, `WaitGroup`, `Once`, atomics, channels |
| `std::task` | `go`, `Task`, `scope`, cancellation `Context`, runtime config |
| `std::time` | `Instant`, `Duration`, timers, `after`, `tick` |
| `std::fmt`, `std::str` | formatting, interpolation support, parsing |
| `std::encoding` | JSON, base64, hex, UTF-8/16 |
| `std::crypto` | hashes and TLS, via vetted bindings at first |
| `std::os`, `std::process`, `std::env` | process, signals, environment |
| `std::testing` | assertions, benchmarks, fuzzing hooks |
| `std::log` | structured logging |

## 9. Open Questions for the User

Resolved in this revision: explicit `return`, the canonical formatter, the `.kn` extension, `<T>` generics, `:=` declarations, and channel `<-` syntax.

1. **Statement terminators**: automatic semicolon insertion as drafted, or mandatory `;`?
2. **Import keyword**: `import` (Go-like) or `use` (Rust-like)? The draft uses `import` and keeps `use` reserved.
3. **Macros**: none for 1.0 (built-in `println` and derive only), hygienic macros, or Zig-style `comptime`?
4. **Task cancellation**: Go-style `Context` passed explicitly, or structured cancellation tied to `scope`?
5. **Panic policy**: unwind (recoverable at task boundaries) or abort by default?
6. **Runtime in `no_std`**: should embedded targets get a minimal single-threaded scheduler?
7. **Bootstrapping**: should the compiler eventually self-host in Kiln?
8. **License and governance**: MIT/Apache-2.0 dual license?

## 10. Canonical Formatting: `kiln fmt` (decided)

`kiln fmt` works like `gofmt`: there is exactly one style, with no config file and no flags that change the output.

- **Indentation**: one tab per level, for leading indentation only. Alignment inside a line uses spaces.
- **Line width**: no hard limit, so long lines are never auto-wrapped. A width of 100 columns (tabs counted as 4) is used only to decide when to break argument lists the user already split across lines.
- **Braces**: Go-style. An opening `{` always goes on the same line as its header (`fn`, `if`, `for`, `struct`, `match`, and so on), and `} else {` stays on one line. Braces are mandatory on every `if`, `for`, and `while` body.
- **Bodies**:
  - Function bodies always span multiple lines.
  - Empty bodies may be written `{}`.
  - `if`/`else` used as a value may stay on one line when it fits.
- **Commas**: these are written the way gofmt writes them.
  - Every comma-separated list must have a trailing comma when it spans multiple lines, and must not have one when it fits on one line. This covers struct fields, enum variants, match and `select` arms (block arms included), struct literals, arguments, parameters, array and tuple literals, and import lists.
  - Single-line lists use `, ` (as in `Rect { w: f64, h: f64 }`).
  - `kiln fmt` adds and removes trailing commas to match these rules, and the parser accepts either form.
- **Spacing**:
  - Binary operators get a single space on each side.
  - No space goes before `(` in calls, and none follows `<-` in a receive.
  - Comments get one space after `//`.
- **Blank lines**:
  - At most one blank line in a row.
  - One blank line between top-level items and between methods.
- **Imports**: within each group, imports are sorted (std first, then third-party, then the local package), and duplicates are merged.
- **Semicolons**: the formatter removes them where a newline already ends the statement.
- **Enforcement**: `kiln build` stays silent about formatting, while `kiln fmt --check` exits non-zero for CI.

## 11. Implementation Roadmap

The compiler is written in Rust as a Cargo workspace:

```
kiln/
  crates/
    kiln_lexer/   kiln_parser/   kiln_ast/    kiln_resolve/
    kiln_types/   kiln_borrowck/ kiln_mir/    kiln_codegen_llvm/
    kiln_runtime/ kiln_driver/   kiln_fmt/    kiln_pkg/
    kiln_ls/      kiln_diagnostics/
  std/            (the Kiln standard library, written in Kiln)
  editors/        tree-sitter-kiln/  vscode-kiln/
```

### Compiler pipeline

1. **Lexer**:
   - Hand-written, producing tokens with spans.
   - Handles automatic semicolon insertion.
   - Lossless: trivia is kept for the formatter and language server.
2. **Parser**:
   - Recursive descent with Pratt parsing for expressions.
   - Error-recovering, producing a lossless concrete syntax tree (CST) shared with the IDE in a rowan-style design.
3. **AST**: a typed AST lowered from the CST, with desugaring of `for`, `?`, `go`, `select`, and interpolation.
4. **Name resolution**: module tree, imports, and visibility, producing HIR.
5. **Type checking**:
   - Trait solving and method resolution.
   - Inference with unification.
   - Coherence and orphan checks.
6. **MIR lowering**:
   - A CFG of basic blocks with explicit drops and moves.
   - Pattern-match compilation into decision trees.
7. **Borrow check**: on MIR, using NLL (liveness plus region inference). It also checks moves, initialization, and missing `return` on non-unit functions.
8. **MIR optimizations**: constant propagation, inlining small functions, and drop elaboration.
9. **Codegen**:
   - Uses LLVM through `inkwell`.
   - Monomorphization happens at this stage.
   - Emits debug info (DWARF).
   - Links with system `lld` or `cc`.
10. **Incremental and query-based**: a `salsa` database so the compiler and language server share one engine.

### Runtime (`kiln_runtime`, written in Rust then ported over time)

- An M:N work-stealing scheduler like Go's G-M-P model:
  - Each OS thread has a run queue, and idle threads steal work from others.
  - Task stacks are growable segmented stacks, or fixed 64 KB stacks with guard pages for the MVP.
  - Context switching is cooperative, with yield points at channel operations, I/O, and loop back-edges (preemption is planned).
- Netpoller: epoll, kqueue, or IOCP.
- Timers: a timer heap.
- Channels: lock-free fast path with parked-task wait queues. `select` follows Go's randomized fair choice.

### Tooling

- **`kiln fmt`**: CST-based with a single style and no options.
- **`kiln` package manager**: resolver (PubGrub), registry client, lockfile, and build caching.
- **`kiln-ls` language server** (the rust-analyzer equivalent):
  - Implements LSP on top of the salsa query database.
  - Features: diagnostics, go-to-definition, find references, hover types, completion, rename, inlay hints (inferred types and lifetimes), code actions such as "add `&`" or "add lifetime", and semantic tokens.
- **Tree-sitter grammar** (`tree-sitter-kiln`): used for highlighting in Neovim, Helix, Zed, and GitHub, with `highlights.scm` and `indents.scm`.
- **VS Code extension** (`vscode-kiln`):
  - A TextMate grammar provides baseline highlighting.
  - Semantic tokens come from `kiln-ls`.
  - Also includes snippets, bracket and comment configuration, and a debugger launch config via CodeLLDB.

### Milestones

| Phase | Deliverable |
|---|---|
| M0 | Spec frozen for v0.1; lexer and parser with error recovery; TextMate and Tree-sitter grammars |
| M1 | Name resolution and type checking for functions, structs, enums, and match; LLVM codegen; "hello world" and fib run |
| M2 | Traits, generics (monomorphization), `Result`, `?`, and `Drop` |
| M3 | MIR and borrow checker (NLL), with lifetimes and elision |
| M4 | Runtime: `go`, channels, `select`, scoped tasks, netpoller; `Send` and `Sync` enforcement |
| M5 | Standard library core (io, fs, net, collections, sync, time); `kiln` build tool and package manager |
| M6 | `kiln-ls` MVP (diagnostics, hover, go-to-definition, completion); `kiln fmt` |
| M7 | `std::http`, JSON, testing framework; a real backend service written in Kiln as the dogfood test |
| M8 | Hardening: fuzzing the compiler, a conformance test suite, performance work, docs, 1.0 stability guarantees |
