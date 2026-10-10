# The standard library

Kiln's standard library covers the ground of Go's: time, concurrency, files, processes, networking, and HTTP. Its APIs follow Rust's lead:

- **Errors are values.** Anything that can fail returns `Result<T, io::Error>`, so `?` passes failures up. An `io::Error` has a `kind()` to match on (`io::ErrorKind::NotFound`) and a message that says what happened: `open config.toml: No such file or directory`.
- **Resources are owned.** A `File`, `TcpStream`, or `TcpListener` closes when it is dropped. There is no `close` to forget.
- **Sharing is explicit.** Values move into tasks; data shared between tasks goes in an `Arc`, behind a `Mutex` or `RwLock` when it changes.
- **Blocking is cheap.** Sleeping, waiting for a lock, and network I/O park only the calling task. The scheduler runs other tasks meanwhile, as goroutines do in Go.

The standard library is written in Kiln, on top of a small C runtime for system calls, timers, and the network poller. You can read its source in `crates/kiln_resolve/std/`.

Phase 2 added text and data modules: `strings`, `strconv`, `bytes`, `collections`, `math`, `rand`, `errors`, `flag`, `encoding` (JSON, base64, hex, CSV), `crypto` (SHA-256, SHA-1, MD5, HMAC), `regexp`, and `context`, plus `kiln test` and network timeouts. Some phase 2 modules (`strings`, `strconv`, `math`, `rand`, `errors`, JSON, base64, hex) are described in the language specification but don't have sections here yet. 🚧 Next: HTTPS, `compress`, `mime`, `url`, and templates.

| Module | What it is for |
|---|---|
| [`std::time`](#stdtime) | durations, clocks, sleeping, timers |
| [`std::sync`](#stdsync) | `Arc`, `Mutex`, `RwLock`, `WaitGroup`, `Once`, atomics |
| [`std::io`](#stdio) | `Read`, `Write`, buffering, standard streams, errors |
| [`std::bytes`](#stdbytes) | byte-slice helpers and `Buffer` |
| [`std::crypto`](#stdcrypto) | SHA-256, SHA-1, MD5, HMAC |
| [`std::encoding::csv`](#stdencodingcsv) | reading and writing CSV |
| [`std::flag`](#stdflag) | command-line flags |
| [`std::regexp`](#stdregexp) | regular expressions |
| [`std::context`](#stdcontext) | cancellation and deadlines |
| [`std::fs`](#stdfs) | files and directories |
| [`std::path`](#stdpath) | path manipulation |
| [`std::os`](#stdos) | arguments, environment, exit, running programs |
| [`std::net`](#stdnet) | TCP and UDP |
| [`std::net::http`](#stdnethttp) | HTTP/1.1 server and client |
| [`std::log`](#stdlog) | leveled logging |
| [`std::fmt`](#stdfmt) | `Display` and `Debug` |
| [`std::collections`](#stdcollections) | `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `VecDeque` |
| [`std::mem`, `std::ptr`](#stdmem-and-stdptr) | low-level memory operations |

## std::time

| Item | Description |
|---|---|
| `time::nanos(n)`, `micros`, `millis`, `secs`, `minutes`, `hours`, `secs_f64(x)` | make a `Duration` |
| `Duration` | `as_millis()`, `as_secs()`, `as_secs_f64()`, `add`, `sub`, `mul`, `div`, `is_zero()`; displays as `1.5s`, `2h3m0s`, `250ms` |
| `Instant::now()` | a monotonic clock reading; `elapsed()`, `since(earlier)`, `add(d)` |
| `SystemTime::now()` | the wall clock; `unix()`, `unix_millis()`, `unix_nanos()`, `utc()`; displays as RFC 3339 (`2026-10-10T15:24:05Z`) |
| `time::sleep(d)` | park this task for `d` |
| `time::after(d)` | a channel that receives once after `d` |
| `time::tick(d)` | a channel that receives every `d` |

```kiln
import std::time

fn main() {
	start := time::Instant::now()
	time::sleep(time::millis(20))
	println("slept {start.elapsed().as_millis()}ms")

	ticks := time::tick(time::millis(10))
	stop := time::after(time::millis(55))
	n mut := 0
	loop {
		select {
			<-ticks => {
				n += 1
			},
			<-stop => break,
		}
	}
	println("{n} ticks; now {time::SystemTime::now()}")
}
```

## std::sync

| Item | Description |
|---|---|
| `Arc<T>` | shared ownership across tasks: `Arc::new(v)`, `clone()`, `count()`; dereferences to `T` |
| `Mutex<T>` | `lock()` returns a guard that dereferences to `T` and unlocks when dropped; `try_lock()` |
| `RwLock<T>` | many readers (`read()`) or one writer (`write()`) |
| `WaitGroup` | `add(n)`, `done()`, `wait()`: wait for a group of tasks |
| `Once` | `call(f)` runs `f` the first time only |
| `AtomicI64`, `AtomicUsize`, `AtomicBool` | `load`, `store`, `add`, `sub`, `swap`, `compare_and_swap` |

Waiting for a lock or a wait group parks the task instead of blocking the thread.

```kiln
import std::sync::{Arc, Mutex, WaitGroup}

fn main() {
	totals := Arc::new(Mutex::new(Vec::new()))
	wg := Arc::new(WaitGroup::new())
	for id in 0..4 {
		t := totals.clone()
		w := wg.clone()
		w.add(1)
		go move {
			sum mut := 0
			for i in 0..1000 {
				sum += i * id
			}
			t.lock().push(sum)
			w.done()
		}
	}
	wg.wait()
	println("{totals.lock().len()} results")
}
```

## std::io

| Item | Description |
|---|---|
| `trait Read` | `read(&mut buf)`; provided: `read_to_end`, `read_to_string`, `read_exact` |
| `trait Write` | `write(buf)`, `flush()`; provided: `write_all`, `write_str` |
| `BufReader::new(r)` | buffered reading: `next_line()` (without the newline, `None` at the end), `read_line(&mut s)`, `read_until(byte, &mut v)`, `peek(n)`, `read_byte()`, `read_char()`, `discard(n)`, `into_inner()` |
| `BufWriter::new(w)` | buffered writing; flushes when dropped; `buffered()`, `available()` |
| `Scanner::new(r)` | Go's `bufio.Scanner`: `next()` returns the next token (`None` at the end); `split(Split::Lines / Words / Chars)`; `max_token(n)` caps token size (64 KiB default); `collect_all()` |
| `Cursor` | an in-memory reader and writer over a `Vec<u8>` |
| `io::copy(&mut r, &mut w)` | copy everything from a reader to a writer |
| `io::stdin()`, `stdout()`, `stderr()`, `io::read_line()` | the standard streams |
| `io::Error` | `kind()`, `os_code()`, `context(what)`, displays its message; `Error::new(kind, msg)`, `Error::other(msg)` |
| `io::ErrorKind` | `NotFound`, `PermissionDenied`, `ConnectionRefused`, `AlreadyExists`, `TimedOut`, `UnexpectedEof`, `InvalidInput`, `InvalidData`, ... |

`&mut R` is a reader whenever `R` is, and likewise for writers, so you can lend a stream to a `BufReader` and keep using it afterward.

```kiln
import std::io
import std::io::{BufReader, BufWriter, Write}

fn main() -> Result<(), io::Error> {
	input := io::Cursor::from_str("first\nsecond\n")
	r mut := BufReader::new(input)
	out mut := BufWriter::new(io::stdout())
	while let Some(line) = r.next_line()? {
		out.write_str("> {line}\n")?
	}
	out.flush()?
	return Ok(())
}
```

## std::bytes

Functions mirror Go's `bytes` package and work on `&[u8]` (write a byte slice literal as `b"text"`, or convert a string with `s.as_bytes()`). Slices that come back borrow from the input.

| Item | Description |
|---|---|
| `index`, `last_index`, `index_byte` | position of a sub-slice or byte, as `Option<usize>` |
| `contains`, `count`, `equal`, `equal_fold` | searching and comparing (`equal_fold` ignores ASCII case) |
| `trim_space`, `trim(b, set)`, `strip_prefix`, `strip_suffix` | trimming |
| `split(b, sep)`, `fields(b)`, `cut(b, sep)` | splitting into borrowed pieces |
| `join(parts, sep)`, `repeat`, `replace_all`, `to_lower`, `to_upper` | building new `Vec<u8>`s |
| `Buffer` | written at the end, read from the front: `write_byte`, `write_char`, `read_byte`, `read_until`, `next(n)`, `len`, `reset`, `to_string_lossy`; implements `io::Read` and `io::Write` |

## std::crypto

| Item | Description |
|---|---|
| `sha256::sum(b)`, `sha256::hex(b)` | SHA-256 digest as `[u8; 32]` or lowercase hex |
| `sha256::Sha256` | streaming: `new()`, `update(b)`, `finish()`, `reset()`; implements `io::Write` |
| `sha1`, `md5` | the same API (`Sha1`, `Md5`); both are broken for security, so use them only for checksums and compatibility |
| `hmac::sha256(key, msg)`, `hmac::sha1(key, msg)` | HMAC tags; `hmac::verify_sha256(key, msg, tag)` checks in constant time |
| `crypto::constant_time_eq(a, b)` | compare secrets without leaking where they differ |

```kiln
import std::crypto::sha256
import std::fs
import std::io

fn main() -> Result<(), io::Error> {
	f mut := fs::File::open("data.bin")?
	h mut := sha256::Sha256::new()
	io::copy(&mut f, &mut h)?
	println(std::encoding::hex::encode(&h.finish()))
	return Ok(())
}
```

## std::encoding::csv

RFC 4180 CSV, like Go's `encoding/csv`. Quoted fields can hold commas, newlines, and `""` for a quote; lines end in `\n` or `\r\n`; empty lines are skipped.

| Item | Description |
|---|---|
| `csv::parse(text)` | every record of an in-memory document, as `Vec<Vec<String>>` |
| `csv::format(rows)` | the reverse, quoting only where needed |
| `Reader::new(r)` | streams from any `io::Read`: `next_record()` (`None` at the end), `read_all()`, `line()`; settings `delimiter`, `comment`, `flexible` (allow differing field counts), `trim_leading_space` |
| `Writer::new(w)` | `write_record(&["a", "b"])`, `write_strings(&vec)`, `flush()`; settings `delimiter`, `use_crlf` |
| `csv::Error` | `Io(io::Error)`, `Parse(line, column, message)`, `FieldCount(line, got, expected)`; `?` converts `io::Error` into it |

```kiln
import std::encoding::csv
import std::fs

fn main() -> Result<(), csv::Error> {
	r mut := csv::Reader::new(fs::File::open("people.csv")?)
	while let Some(rec) = r.next_record()? {
		println("{rec[0]} is {rec[1]}")
	}
	return Ok(())
}
```

## std::flag

Command-line flags with Go's syntax: `-name value`, `-name=value`, `--name value`, and bare `-v` for booleans. Parsing stops at the first positional argument or `--`.

| Item | Description |
|---|---|
| `FlagSet::new(name)` | an empty set; `name` appears in the usage text |
| `bool`, `int`, `float`, `string` `(name, default, usage)` | define flags (integers accept `0x`, `0o`, `0b`) |
| `parse(&args)` | returns `Result<(), flag::Error>` (`Help`, `Unknown`, `MissingValue`, `Invalid`) |
| `parse_or_exit(&args)` | prints the error and usage and exits with status 2 (0 for `-h`) |
| `get_bool`, `get_int`, `get_float`, `get_string` | values after parsing |
| `args()`, `is_set(name)`, `usage()` | positional arguments, whether a flag was given, the help text |

```kiln
import std::flag
import std::os

fn main() {
	fs mut := flag::FlagSet::new("greet")
	fs.string("name", "world", "who to greet")
	fs.int("times", 1, "how many greetings")
	args := os::args()
	fs.parse_or_exit(&args[1..])
	for _ in 0..fs.get_int("times") {
		println("hello, {fs.get_string("name")}")
	}
}
```

## std::regexp

Regular expressions with Go's RE2 semantics: matching always takes time linear in the input, because there are no backreferences or lookaround. Offsets are byte offsets.

| Item | Description |
|---|---|
| `Regex::new(pattern)`, `Regex::must(pattern)` | compile (`Result`), or panic on a bad pattern |
| `is_match(text)`, `find(text)`, `find_at(text, i)`, `find_str(text)` | the first match (`Match { start, end }`) |
| `find_all`, `find_all_str` | all non-overlapping matches |
| `captures(text)`, `captures_all(text)` | groups: `get(i)`, `name("year")`, `pos(i)`, `len()` |
| `replace_all(text, "$2-$1")`, `replace_all_fn(text, \|c\| ...)`, `replace_all_literal` | replacements (`$1`, `${name}`, `$$`); write `${name}` templates as raw strings (`r"..."`), since `{...}` in a normal string is interpolation |
| `split(text)`, `num_groups()`, `group_names()`, `as_str()` | other helpers |
| `regexp::quote_meta(s)`, `regexp::is_match(pattern, text)` | escape a literal; one-off matching |

Syntax: `.` `[a-z]` `[^...]` `\d \w \s` (and `\D \W \S`) `^ $ \A \z \b \B` `( )` `(?: )` `(?P<name> )` `|` `* + ? {n} {n,} {n,m}` (add `?` for lazy), and the flags `(?i)`, `(?m)`, `(?s)`.

```kiln
import std::regexp::Regex

fn main() {
	date := Regex::must(r"(?P<y>\d{4})-(?P<m>\d{2})-(?P<d>\d{2})")
	println(date.replace_all("due 2026-10-10", r"${d}/${m}/${y}"))   // due 10/10/2026
}
```

## std::context

Cancellation and deadlines across tasks, like Go's `context`. A `Context` is a cheap handle: clone it into tasks. Cancelling a context cancels every context derived from it.

| Item | Description |
|---|---|
| `context::background()` | the root: never cancelled |
| `with_cancel(&parent)`, `with_timeout(&parent, d)`, `with_deadline(&parent, at)` | derived contexts (a child's deadline never exceeds its parent's) |
| `ctx.cancel()` | cancel it and its children (calling it again does nothing) |
| `ctx.done()` | a channel that receives the reason (`Canceled` or `DeadlineExceeded`); use it in `select` |
| `ctx.err()`, `ctx.check()?`, `ctx.is_done()`, `ctx.wait()` | inspect or wait |
| `ctx.deadline()`, `ctx.remaining()` | for socket timeouts: `conn.set_read_timeout(ctx.remaining())?` |
| `net::TcpStream::connect_ctx(addr, &ctx)` | dial within the deadline |

```kiln
import std::context
import std::time

fn main() {
	ctx := context::with_timeout(&context::background(), time::millis(50))
	(tx, rx) := chan::<i32>(1)
	go move {
		time::sleep(time::secs(1))
		tx <- 42
	}
	select {
		v := <-rx => println("result {v}"),
		e := <-ctx.done() => println("gave up: {e}"),   // gave up: context deadline exceeded
	}
}
```

## std::fs

| Item | Description |
|---|---|
| `fs::read_to_string(path)`, `fs::read(path)` | read a whole file |
| `fs::write_string(path, s)`, `fs::write(path, bytes)`, `fs::append_string(path, s)` | write a whole file |
| `File::open(path)`, `File::create(path)` | open for reading, or create/truncate for writing; `File` implements `Read` and `Write`; `seek(SeekFrom::Start(n))`, `sync()` |
| `OpenOptions::new().append(true).create(true).open(path)` | other ways to open: `read`, `write`, `append`, `create`, `truncate`, `create_new` |
| `fs::read_dir(dir)` | entries sorted by name: `name`, `path`, `is_dir()`, `metadata()` |
| `fs::metadata(path)` | `is_file()`, `is_dir()`, `len()`, `modified()`; `symlink_metadata` doesn't follow links |
| `fs::create_dir`, `create_dir_all`, `remove_file`, `remove_dir`, `remove_dir_all`, `rename`, `copy` | manage files and directories |
| `fs::exists`, `is_file`, `is_dir` | quick checks |

File operations block their worker thread while the OS works, as in Go; the other workers keep running tasks.

```kiln
import std::fs
import std::io
import std::os
import std::path

fn main() -> Result<(), io::Error> {
	dir := path::join(&os::temp_dir(), "kiln-demo")
	fs::create_dir_all(&dir)?
	notes := path::join(&dir, "notes.txt")
	fs::write_string(&notes, "buy clay\n")?
	fs::append_string(&notes, "fire kiln\n")?
	for line in fs::read_to_string(&notes)?.lines() {
		println("- {line}")
	}
	match fs::read_to_string(&path::join(&dir, "missing.txt")) {
		Ok(_) => println("found it?"),
		Err(e) => println("as expected: {e}"),
	}
	fs::remove_dir_all(&dir)?
	return Ok(())
}
```

## std::path

Paths are strings. `/` separates elements on every OS; on Windows `\` does too, and `join` uses `\`.

| Function | Example |
|---|---|
| `path::join(a, b)`, `join_all(&[...])` | `join("logs", "app.txt")` is `logs/app.txt` |
| `path::base(p)` | `base("/a/b/c.txt")` is `c.txt` |
| `path::dir(p)` | `dir("/a/b/c.txt")` is `/a/b`; `dir("c.txt")` is `.` |
| `path::ext(p)`, `stem(p)` | `ext("x.tar.gz")` is `.gz`, `stem("x.tar.gz")` is `x.tar` |
| `path::clean(p)` | `clean("a/./b/../c//")` is `a/c` |
| `path::is_absolute(p)`, `separator()` | |

## std::os

| Item | Description |
|---|---|
| `os::args()` | command-line arguments; the first is the program |
| `os::getenv(name)`, `setenv`, `unsetenv`, `environ()` | environment variables |
| `os::exit(code)` | exit now (flushes output; `defer` and destructors do not run) |
| `os::current_dir()`, `set_current_dir(p)`, `temp_dir()`, `executable()`, `hostname()`, `pid()` | process information |
| `os::name()`, `is_windows()` | `"linux"`, `"macos"`, or `"windows"` |
| `Command::new(prog).arg(a).args(&[...])` | describe a program to run; `status()` runs it with this process's streams, `output()` captures them |
| `Output` | `status`, `stdout`, `stderr` (bytes), `success()`, `stdout_text()`, `stderr_text()` |

```kiln
import std::io
import std::os

fn main() -> Result<(), io::Error> {
	args := os::args()
	if args.len() < 2 {
		eprintln("usage: {args[0]} <program> [args...]")
		os::exit(2)
	}
	cmd mut := os::Command::new(&args[1])
	for a in &args[2..] {
		cmd = cmd.arg(a)
	}
	out := cmd.output()?
	print(out.stdout_text())
	println("exit status {out.status}")
	return Ok(())
}
```

## std::net

Addresses are `"host:port"` strings: `"127.0.0.1:8080"`, `"localhost:80"`, `"[::1]:443"`, or `":8080"` for every interface. Host names go through the OS resolver. Accepting, reading, and writing park the task until the socket is ready.

| Item | Description |
|---|---|
| `TcpListener::bind(addr)` | `accept()`, `incoming()` (a channel of connections), `local_addr()`, `port()`, `set_accept_timeout(Some(d))` |
| `TcpStream::connect(addr)`, `TcpStream::connect_timeout(addr, d)` | `read`, `write`, `write_all`, `write_str` (through `&self`, so one task can read while another writes), `shutdown`, `peer_addr()`, `local_addr()`, `set_read_timeout(Some(d))`, `set_write_timeout(...)`; implements `Read` and `Write` |
| `UdpSocket::bind(addr)` | `send_to(buf, addr)`, `recv_from(&mut buf)` returns `(n, from)`, `local_addr()`, `set_read_timeout`, `set_write_timeout` |
| `net::split_host_port(addr)` | `"host:80"` becomes `("host", 80)` |

Timeouts work as in Rust: each wait that takes longer than the timeout fails with `io::ErrorKind::TimedOut`, and the socket stays usable. `None` (the default) waits forever. The timer parks only the waiting task. `connect_timeout` covers connecting to all resolved addresses together; resolving the host name is not covered yet.

```kiln
import std::io
import std::io::BufReader
import std::net::{TcpListener, TcpStream}

fn echo(conn: TcpStream) -> Result<(), io::Error> {
	r mut := BufReader::new(&conn)
	while let Some(line) = r.next_line()? {
		conn.write_str("{line}\n")?
	}
	return Ok(())
}

fn main() -> Result<(), io::Error> {
	listener := TcpListener::bind("127.0.0.1:7000")?
	for conn in listener.incoming() {
		c := conn?
		go move {
			if let Err(e) = echo(c) {
				eprintln("echo: {e}")
			}
		}
	}
	return Ok(())
}
```

## std::net::http

An HTTP/1.1 server and client. Each connection gets its own task; connections are kept alive between requests. 🚧 HTTPS is not supported yet.

| Item | Description |
|---|---|
| `Router::new()` | `get(pattern, handler)`, `post`, `put`, `delete`, `handle` (any method), `listen(addr)`, `serve(listener)` |
| patterns | `"/users"` matches exactly; `"/static/"` matches everything under it; the longest match wins; unmatched paths get 404, wrong methods 405 |
| handlers | functions or closures from `Request` to `Response` |
| `http::serve(addr, handler)` | serve every path with one handler |
| `Server::new(router)` | a router plus timeouts: `with_read_header_timeout(d)` (default 10s), `with_read_timeout(d)` (60s, whole request), `with_write_timeout(d)` (60s, the response), `with_idle_timeout(d)` (120s between keep-alive requests); each takes `Some(duration)` or `None`; `listen(addr)`, `serve(listener)`. `Router::listen`/`serve` use these defaults. A client that runs out of time is disconnected |
| `Client::new()` | `with_timeout(d)` bounds a whole exchange (connect, send, full response; `ErrorKind::TimedOut` when exceeded; default none), `with_connect_timeout(d)` (default 30s); `get`, `post`, `send` |
| `Request` | `method`, `target`, `headers`, `body`; `path()`, `query()`, `query_param(name)`, `header(name)`, `body_text()` |
| `Response` | `Response::text(status, s)`, `html`, `json`, `bytes`, `new(status)`, `not_found()`, `redirect(url)`, `with_header(k, v)`; `status`, `headers`, `body`, `body_text()` |
| `Headers` | case-insensitive: `get`, `set`, `add`, `remove`, `has`, `entries()` |
| `http::get(url)`, `post(url, content_type, body)`, `send(request)` | the client with a default `Client`: plain `http://` URLs; chunked and fixed-length bodies |
| `http::escape(s)`, `unescape(s)`, `status_text(code)` | helpers |

```kiln
import std::io
import std::net::http
import std::sync::{Arc, AtomicI64}

fn hello(req: http::Request) -> http::Response {
	name := req.query_param("name").unwrap_or("world".to_string())
	return http::Response::text(200, "hello, {name}!\n")
}

fn main() -> Result<(), io::Error> {
	visits := Arc::new(AtomicI64::new(0))
	router mut := http::Router::new()
	router.get("/hello", hello)
	router.get("/visits", move |req: http::Request| -> http::Response {
		n := visits.add(1)
		return http::Response::json(200, "{{\"visits\": {n}}}")
	})
	return router.listen(":8080")
}
```

## std::log

`log::debug`, `info`, `warn`, and `error` print a timestamped line to stderr; `log::fatal` also exits with status 1. Messages below the level are dropped. The level starts at `Info`; change it with `log::set_level(log::Level::Debug)` or the `KILN_LOG` environment variable (`debug`, `info`, `warn`, `error`).

```text
2026-10-10T15:24:05Z INFO listening on :8080
```

## std::fmt

Formatting is string interpolation: `"{x}"` uses `Display`, and `"{x:?}"` uses `Debug`. Implement `Display` (its method is `to_string`) to give a type a user-facing form:

```kiln
import std::fmt

struct Point {
	x: f64,
	y: f64,
}

impl fmt::Display for Point {
	fn to_string(&self) -> String {
		return "({self.x}, {self.y})"
	}
}

fn main() {
	p := Point { x: 1.5, y: -2.0 }
	println("p = {p}")
}
```

## std::collections

| Item | Description |
|---|---|
| `HashMap<K, V>` | hash table; keys need `Hash + Eq`. `insert`, `get`, `get_mut`, `contains_key`, `remove`, `entry(k).or_insert(v)` / `or_insert_with(f)` / `and_modify(f)`, `retain`, `extend`, `iter`, `keys`, `values`, `values_mut`, `into_iter`, `drain`, `HashMap::from_iter(pairs)` |
| `HashSet<T>` | `insert` (returns whether it was new), `contains`, `remove`, `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_disjoint` |
| `BTreeMap<K, V>` | sorted map (a B-tree); keys need `Ord`. The `HashMap` methods plus `first_key_value`, `last_key_value`, `pop_first`, `pop_last`, `range(from, to)`, `range_from(from)`, `split_off(k)`; iterates in key order |
| `BTreeSet<T>` | sorted set: `first`, `last`, `range`, and the set operations, in order |
| `VecDeque<T>` | ring buffer: `push_back`, `push_front`, `pop_back`, `pop_front`, `front`, `back`, `get`, `insert`, `remove`, `rotate_left`, `retain`, `iter`, `iter_mut` |

Lookups take any borrowed form of the key, so a `HashMap<String, V>` is
searched with a `&str` and no allocation. `HashMap` iteration order is
unspecified; use `BTreeMap` when order matters. Maps and sets implement
`Eq` (independent of order for hash tables), `Clone`, and `Debug`.

```kiln
import std::collections::{BTreeMap, HashMap}

fn main() {
    counts mut := HashMap::new()
    for w in "the cat and the hat".split(" ") {
        *counts.entry(w.to_string()).or_insert(0) += 1
    }
    println("the: {counts.get("the"):?}")

    sorted mut := BTreeMap::new()
    for (w, n) in counts.iter() {
        sorted.insert(w.clone(), *n)
    }
    println("{sorted:?}")  // {"and": 1, "cat": 1, "hat": 1, "the": 2}
}
```

## std::mem and std::ptr

`mem::swap(&mut a, &mut b)`, `mem::replace(&mut a, v)`, `mem::forget(v)`, `mem::size_of::<T>()`, and `mem::align_of::<T>()`. `std::ptr` has raw pointer operations (`null`, `read`, `write`, `add`, `copy`, `drop_in_place`, ...) for `unsafe` code; the standard library uses them to build `Arc` and the locks.
