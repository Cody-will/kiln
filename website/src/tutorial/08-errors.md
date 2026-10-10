# Errors with `Result` and `?`

Kiln has no exceptions. A function that can fail returns `Result<T, E>`, which is either `Ok(value)` or `Err(error)`:

```kiln
fn parse_port(s: &str) -> Result<u16, String> {
	if s.is_empty() {
		return Err(String::from("empty port"))
	}
	return Ok(8080)
}

fn main() {
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
	host: String,
	port: u16,
}

fn parse_port(s: &str) -> Result<u16, String> {
	if s.is_empty() {
		return Err(String::from("empty port"))
	}
	return Ok(8080)
}

fn load(host: &str, port: &str) -> Result<Config, String> {
	p := parse_port(port)?      // returns early on error
	return Ok(Config { host: host.to_string(), port: p })
}

fn main() {
	match load("localhost", "8080") {
		Ok(cfg) => println("{cfg.host}:{cfg.port}"),
		Err(e) => eprintln("config error: {e}"),
	}
}
```

Errors bubble up to the caller that knows how to handle them, so a single `match` near the top can handle errors from deep inside.

`?` also works on `Option<T>` in a function that returns `Option`.

## Converting error types

When the error types differ, `?` converts them with the `From` trait:

```kiln
struct IoError {
	code: i32,
}

enum AppError {
	Io(IoError),
	Invalid(String),
}

impl From<IoError> for AppError {
	fn from(e: IoError) -> AppError {
		return AppError::Io(e)
	}
}

fn read_file() -> Result<String, IoError> {
	return Err(IoError { code: 2 })
}

fn run() -> Result<(), AppError> {
	text := read_file()?    // IoError → AppError via From
	if text.is_empty() {
		return Err(AppError::Invalid(String::from("empty file")))
	}
	return Ok(())
}
```

Without the `impl From`, the checker reports:

```text
error: `?` couldn't convert the error to `AppError`
  = help: the error type is `IoError`; implement `From<IoError> for AppError`
```

## `main` can return a `Result`

```kiln
fn main() -> Result<(), String> {
	return Ok(())
}
```

If `main` returns `Err`, the program prints the error and exits with status 1.

## Panics

`panic("message")` stops the program. Use it only for bugs ("this can't happen"), not for errors a caller could handle. `.unwrap()` and `.expect("why")` on `Option`/`Result` panic on `None`/`Err`.
