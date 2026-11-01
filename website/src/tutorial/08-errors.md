# Errors with `result` and `?`

Kiln has no exceptions. A function that can fail returns `result<T, E>`, which is either `Ok(value)` or `Err(error)`:

```kiln
fun parse_port(s &str) result<u16, string> {
	if s.is_empty() {
		return Err(string::from("empty port"))
	}
	return Ok(8080)
}

fun main() {
	match parse_port("8080") {
		Ok(p) => println("port {p}"),
		Err(e) => eprintln("bad port: {e}"),
	}
}
```

## The `?` operator

Checking every call by hand is what Go's `if err != nil` does. In Kiln, `?` does it for you: if the value is `Err`, the function returns that error right away; if it is `Ok`, `?` unwraps it.

```kiln
struct Config {
	host string,
	port u16,
}

fun parse_port(s &str) result<u16, string> {
	if s.is_empty() {
		return Err(string::from("empty port"))
	}
	return Ok(8080)
}

fun load(host &str, port &str) result<Config, string> {
	p := parse_port(port)?      // returns early on error
	return Ok(Config { host: host.to_string(), port: p })
}

fun main() {
	match load("localhost", "8080") {
		Ok(cfg) => println("{cfg.host}:{cfg.port}"),
		Err(e) => eprintln("config error: {e}"),
	}
}
```

Errors bubble up to the caller that knows how to handle them, so a single `match` near the top can handle errors from deep inside.

`?` also works on `option<T>` in a function that returns `option`.

## Converting error types

When the error types differ, `?` converts them with the `From` trait:

```kiln
struct IoError {
	code i32,
}

enum AppError {
	Io(IoError),
	Invalid(string),
}

impl From<IoError> for AppError {
	fun from(e IoError) AppError {
		return AppError::Io(e)
	}
}

fun read_file() result<string, IoError> {
	return Err(IoError { code: 2 })
}

fun run() result<void, AppError> {
	text := read_file()?    // IoError → AppError via From
	if text.is_empty() {
		return Err(AppError::Invalid(string::from("empty file")))
	}
	return Ok()
}
```

Without the `impl From`, the checker reports:

```text
error: `?` couldn't convert the error to `AppError`
  = help: the error type is `IoError`; implement `From<IoError> for AppError`
```

## `main` can return a `result`

```kiln
fun main() result<void, string> {
	return Ok()
}
```

If `main` returns `Err`, the program prints the error and exits with status 1.

## Panics

`panic("message")` stops the program. Use it only for bugs ("this can't happen"), not for errors a caller could handle. `.unwrap()` and `.expect("why")` on `option`/`result` panic on `None`/`Err`.

A server that must survive a bug in one request can contain it with `std::panic::recover`, Kiln's version of Go's `recover`. The panic's `defer`s run, then `recover` returns the message as an `Err`:

```kiln
import std::panic

fun main() {
	r := panic::recover(|| [1, 2, 3][5])
	println("{r:?}") // Err("index out of bounds: the length is 3 but the index is 5")
}
```

Locals in the functions the panic leaves are not dropped (their memory leaks), so reach for `recover` at a boundary such as one request, not as everyday control flow.
