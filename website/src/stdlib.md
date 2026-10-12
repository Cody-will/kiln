# The standard library

Kiln's standard library covers the ground of Go's: time, concurrency, files, processes, networking, and HTTP. Its APIs follow Rust's lead:

- **Errors are values.** Anything that can fail returns `result<T, io::Error>`, so `?` passes failures up. An `io::Error` has a `kind()` to match on (`io::ErrorKind::NotFound`) and a message that says what happened: `open config.toml: No such file or directory`.
- **Resources are owned.** A `File`, `TcpStream`, or `TcpListener` closes when it is dropped. There is no `close` to forget.
- **Sharing is explicit.** Values move into tasks; data shared between tasks goes in an `Arc`, behind a `Mutex` or `RwLock` when it changes.
- **Blocking is cheap.** Sleeping, waiting for a lock, and network I/O park only the calling task. The scheduler runs other tasks meanwhile, as goroutines do in Go.

The standard library is written in Kiln, on top of a small C runtime for system calls, timers, and the network poller. You can read its source in `crates/kiln_resolve/std/`.

Phase 2 added text and data modules: `strings`, `strconv`, `bytes`, `collections`, `math`, `rand`, `errors`, `flag`, `encoding` (JSON, base64, hex, CSV), `crypto` (SHA-256, SHA-1, MD5, HMAC), `regexp`, and `context`, plus `kiln test` and network timeouts. 🚧 Next: `compress`, `mime`, `url`, and templates.

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
| [`std::net::http`](#stdnethttp) | HTTP/1.1 server and client (HTTP and HTTPS) |
| [`std::net::tls`](#stdnettls) | TLS streams |
| [`std::log`](#stdlog) | leveled logging |
| [`std::fmt`](#stdfmt) | `Display` and `Debug` |
| [`std::strings`](#stdstrings) | string building and joining (most operations are `str` methods) |
| [`std::strconv`](#stdstrconv) | numbers to and from text |
| [`std::math`](#stdmath) | floating-point math |
| [`std::rand`](#stdrand) | random numbers, including cryptographic |
| [`std::errors`](#stderrors) | `AnyError` and `.context(...)` |
| [`std::encoding::json`](#stdencodingjson) | JSON values and `@derive(Json)` |
| [`std::encoding::base64`, `hex`](#stdencodingbase64-and-stdencodinghex) | base64 and hex |
| [`std::collections`](#stdcollections) | `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `VecDeque` |
| [`std::mem`, `std::ptr`](#stdmem-and-stdptr) | low-level memory operations |

## std::time

| Item | Description |
|---|---|
| `time::nanos(n)`, `micros`, `millis`, `secs`, `minutes`, `hours`, `secs_f64(x)` | make a `Duration` |
| `Duration` | `as_millis()`, `as_secs()`, `as_secs_f64()`, `add`, `sub`, `mul`, `div`, `is_zero()`; displays as `1.5s`, `2h3m0s`, `250ms` |
| `time::parse_duration(s)` | parse Go-style durations (`300ms`, `-1.5h`, `2h45m`, `1m30.5s`; units `ns` `us`/`µs` `ms` `s` `m` `h`); returns `result<Duration, ParseDurationError>`; every displayed `Duration` parses back |
| `Instant::now()` | a monotonic clock reading; `elapsed()`, `since(earlier)`, `add(d)` |
| `SystemTime::now()` | the wall clock; `unix()`, `unix_millis()`, `unix_nanos()`, `utc()`; displays as RFC 3339 (`2026-10-10T15:24:05Z`) |
| `time::sleep(d)` | park this task for `d` |
| `time::after(d)` | a channel that receives once after `d` |
| `time::tick(d)` | a channel that receives every `d` |

```kiln
import std::time

fun main() {
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

fun main() {
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

fun main() result<void, io::Error> {
	input := io::Cursor::from_str("first\nsecond\n")
	r mut := BufReader::new(input)
	out mut := BufWriter::new(io::stdout())
	while let Some(line) = r.next_line()? {
		out.write_str("> {line}\n")?
	}
	out.flush()?
	return Ok()
}
```

## std::bytes

Functions mirror Go's `bytes` package and work on `&[u8]` (write a byte slice literal as `b"text"`, or convert a string with `s.as_bytes()`). Slices that come back borrow from the input.

| Item | Description |
|---|---|
| `index`, `last_index`, `index_byte` | position of a sub-slice or byte, as `option<usize>` |
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

fun main() result<void, io::Error> {
	f mut := fs::File::open("data.bin")?
	h mut := sha256::Sha256::new()
	io::copy(&mut f, &mut h)?
	println(std::encoding::hex::encode(&h.finish()))
	return Ok()
}
```

## std::encoding::csv

RFC 4180 CSV, like Go's `encoding/csv`. Quoted fields can hold commas, newlines, and `""` for a quote; lines end in `\n` or `\r\n`; empty lines are skipped.

| Item | Description |
|---|---|
| `csv::parse(text)` | every record of an in-memory document, as `Vec<Vec<string>>` |
| `csv::format(rows)` | the reverse, quoting only where needed |
| `Reader::new(r)` | streams from any `io::Read`: `next_record()` (`None` at the end), `read_all()`, `line()`; settings `delimiter`, `comment`, `flexible` (allow differing field counts), `trim_leading_space` |
| `Writer::new(w)` | `write_record(&["a", "b"])`, `write_strings(&vec)`, `flush()`; settings `delimiter`, `use_crlf` |
| `csv::Error` | `Io(io::Error)`, `Parse(line, column, message)`, `FieldCount(line, got, expected)`; `?` converts `io::Error` into it |

```kiln
import std::encoding::csv
import std::fs

fun main() result<void, csv::Error> {
	r mut := csv::Reader::new(fs::File::open("people.csv")?)
	while let Some(rec) = r.next_record()? {
		println("{rec[0]} is {rec[1]}")
	}
	return Ok()
}
```

## std::flag

Command-line flags with Go's syntax: `-name value`, `-name=value`, `--name value`, and bare `-v` for booleans. Parsing stops at the first positional argument or `--`.

| Item | Description |
|---|---|
| `FlagSet::new(name)` | an empty set; `name` appears in the usage text |
| `bool`, `int`, `float`, `string`, `duration` `(name, default, usage)` | define flags (integers accept `0x`, `0o`, `0b`; durations use `time::parse_duration`, e.g. `-timeout 1m30s`) |
| `parse(&args)` | returns `result<void, flag::Error>` (`Help`, `Unknown`, `MissingValue`, `Invalid`) |
| `parse_or_exit(&args)` | prints the error and usage and exits with status 2 (0 for `-h`) |
| `get_bool`, `get_int`, `get_float`, `get_string`, `get_duration` | values after parsing |
| `args()`, `is_set(name)`, `usage()` | positional arguments, whether a flag was given, the help text |

```kiln
import std::flag
import std::os

fun main() {
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
| `Regex::new(pattern)`, `Regex::must(pattern)` | compile (`result`), or panic on a bad pattern |
| `is_match(text)`, `find(text)`, `find_at(text, i)`, `find_str(text)` | the first match (`Match { start, end }`) |
| `find_all`, `find_all_str` | all non-overlapping matches |
| `captures(text)`, `captures_all(text)` | groups: `get(i)`, `name("year")`, `pos(i)`, `len()` |
| `replace_all(text, "$2-$1")`, `replace_all_fn(text, \|c\| ...)`, `replace_all_literal` | replacements (`$1`, `${name}`, `$$`); write `${name}` templates as raw strings (`r"..."`), since `{...}` in a normal string is interpolation |
| `split(text)`, `num_groups()`, `group_names()`, `as_str()` | other helpers |
| `regexp::quote_meta(s)`, `regexp::is_match(pattern, text)` | escape a literal; one-off matching |

Syntax: `.` `[a-z]` `[^...]` `\d \w \s` (and `\D \W \S`) `^ $ \A \z \b \B` `( )` `(?: )` `(?P<name> )` `|` `* + ? {n} {n,} {n,m}` (add `?` for lazy), and the flags `(?i)`, `(?m)`, `(?s)`.

```kiln
import std::regexp::Regex

fun main() {
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

fun main() {
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

fun main() result<void, io::Error> {
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
	return Ok()
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

fun main() result<void, io::Error> {
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
	return Ok()
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

fun echo(conn TcpStream) result<void, io::Error> {
	r mut := BufReader::new(&conn)
	while let Some(line) = r.next_line()? {
		conn.write_str("{line}\n")?
	}
	return Ok()
}

fun main() result<void, io::Error> {
	listener := TcpListener::bind("127.0.0.1:7000")?
	for conn in listener.incoming() {
		c := conn?
		go move {
			if let Err(e) = echo(c) {
				eprintln("echo: {e}")
			}
		}
	}
	return Ok()
}
```

## std::net::http

An HTTP/1.1 server and client, over plain TCP or TLS. Each connection gets its own task; connections are kept alive between requests.

| Item | Description |
|---|---|
| `Router::new()` | `get(pattern, handler)`, `post`, `put`, `delete`, `handle` (any method), `listen(addr)`, `serve(listener)` |
| patterns | `"/users"` matches exactly; `"/static/"` matches everything under it; the longest match wins; unmatched paths get 404, wrong methods 405 |
| handlers | functions or closures from `Request` to `Response` |
| `http::serve(addr, handler)` | serve every path with one handler |
| `Server::new(router)` | a router plus timeouts: `with_read_header_timeout(d)` (default 10s), `with_read_timeout(d)` (60s, whole request), `with_write_timeout(d)` (60s, the response), `with_idle_timeout(d)` (120s between keep-alive requests); each takes `Some(duration)` or `None`; `listen(addr)`, `serve(listener)`. `Router::listen`/`serve` use these defaults. A client that runs out of time is disconnected |
| HTTPS server | `Server::listen_tls(addr, cfg)` / `serve_tls(listener, cfg)` (and `Router::listen_tls`) with a `tls::ServerConfig`; the handshake must finish within the read-header timeout |
| `Client::new()` | `with_timeout(d)` bounds a whole exchange (connect, send, full response; `ErrorKind::TimedOut` when exceeded; default none), `with_connect_timeout(d)` (default 30s), `with_tls_config(cfg)` (trust a private CA); `get`, `post`, `send`; `https://` URLs verify the server's certificate against the Mozilla roots |
| `Request` | `method`, `target`, `headers`, `body`; `path()`, `query()`, `query_param(name)`, `header(name)`, `body_text()` |
| `Response` | `Response::text(status, s)`, `html`, `json`, `bytes`, `new(status)`, `not_found()`, `redirect(url)`, `with_header(k, v)`; `status`, `headers`, `body`, `body_text()` |
| `Headers` | case-insensitive: `get`, `set`, `add`, `remove`, `has`, `entries()` |
| `http::get(url)`, `post(url, content_type, body)`, `send(request)` | the client with a default `Client`: `http://` and `https://` URLs; chunked and fixed-length bodies |
| `http::escape(s)`, `unescape(s)`, `status_text(code)` | helpers |

```kiln
import std::io
import std::net::http
import std::sync::{Arc, AtomicI64}

fun hello(req http::Request) http::Response {
	name := req.query_param("name").unwrap_or("world".to_string())
	return http::Response::text(200, "hello, {name}!\n")
}

fun main() result<void, io::Error> {
	visits := Arc::new(AtomicI64::new(0))
	router mut := http::Router::new()
	router.get("/hello", hello)
	router.get("/visits", move |req http::Request| {
		n := visits.add(1)
		return http::Response::json(200, "{{\"visits\": {n}}}")
	})
	return router.listen(":8080")
}
```

## std::net::tls

Encrypted TCP streams (TLS 1.2 and 1.3). The protocol implementation is [rustls](https://github.com/rustls/rustls) with *ring* cryptography, linked only into programs that use TLS; socket I/O stays in Kiln, so waits park only the task and TCP timeouts apply.

| Item | Description |
|---|---|
| `ClientConfig::new()` | trusts the Mozilla root certificates (webpki-roots) |
| `ClientConfig::with_roots_pem(pem)` | also trusts the PEM certificates given (private CAs) |
| `ClientConfig::insecure_skip_verify()` | accepts any certificate; tests only |
| `ServerConfig::from_pem(cert, key)`, `from_files(cert_path, key_path)` | a certificate chain (leaf first) and a PKCS#8, PKCS#1, or SEC1 key |
| `TlsStream::connect(addr, &cfg)` | dial and handshake, checking the certificate against the host name |
| `TlsStream::client(tcp, host, &cfg)`, `TlsStream::server(tcp, &cfg)` | handshake over an existing connection |
| `TlsStream` | `Read` and `Write` (both need `&mut`), `close()` sends close_notify, `tcp()` for timeouts and addresses, `version()` (12 or 13) |

A peer that closes without close_notify makes `read` return an `UnexpectedEof` error, since the data may have been cut short.

```kiln
import std::io
import std::net::http
import std::net::tls

fun hello(req http::Request) http::Response {
	return http::Response::text(200, "hello over TLS\n")
}

fun main() result<void, io::Error> {
	router mut := http::Router::new()
	router.get("/", hello)
	cfg := tls::ServerConfig::from_files("cert.pem", "key.pem")?
	return router.listen_tls(":8443", cfg)
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
	x f64,
	y f64,
}

impl fmt::Display for Point {
	fun to_string(&self) string {
		return "({self.x}, {self.y})"
	}
}

fun main() {
	p := Point { x: 1.5, y: -2.0 }
	println("p = {p}")
}
```

## std::strings

Most string operations are methods on `str` and `string`: `len`, `is_empty`, `contains`, `starts_with`, `ends_with`, `find`, `split`, `split_once`, `lines`, `trim`, `trim_start`, `trim_end`, `strip_prefix`, `strip_suffix`, `replace`, `repeat`, `to_lower`, `to_upper`, `eq_ignore_ascii_case`, `chars`, `char_indices`, `as_bytes`. Slicing (`&s[2..5]`) works on byte offsets, which must fall on character boundaries. The `strings` module adds what isn't a method:

| Item | Description |
|---|---|
| `strings::join(&parts, sep)` | joins a slice of strings with `sep` |
| `strings::Builder::new()`, `with_capacity(n)` | builds a string piece by piece: `push(s)`, `push_char(c)`, `push_line(s)`, `len()`, `as_str()`, `clear()`, `finish()` |

```kiln
import std::strings

fun main() {
	words := "  the quick  brown fox ".trim().split(" ")
	b mut := strings::Builder::new()
	for w in words {
		if !w.is_empty() {
			b.push(&w.to_upper())
			b.push_char('-')
		}
	}
	println(b.finish())
	println(strings::join(&["a", "b", "c"], ", "))
}
```

## std::strconv

Converting numbers and booleans to and from text. Every integer type also has `T::parse(s)` and `T::from_str_radix(s, radix)`. Parsing accepts an optional sign and `_` between digits, and rejects surrounding spaces.

| Item | Description |
|---|---|
| `parse_int(s)`, `parse_uint(s)` | `result<i64, ParseError>` / `result<u64, ParseError>`; `0x`, `0o`, `0b` prefixes are not accepted here (use `from_str_radix`) |
| `parse_float(s)` | the closest `f64`; accepts `1e-9`, `.5`, `inf`, `NaN` |
| `parse_bool(s)` | exactly `true` or `false` |
| `format_int(v, radix)`, `format_uint(v, radix)` | text in base 2 to 36 |
| `format_float(v)` | the shortest text that parses back to exactly `v` |
| `quote(s)` | `s` as a double-quoted literal with escapes |
| `ParseError` | `kind()` is `Empty`, `InvalidDigit`, or `OutOfRange` |

```kiln
import std::strconv

fun main() {
	match strconv::parse_int("-42") {
		Ok(n) => println("{n + 1}"),
		Err(e) => println("bad: {e}"),
	}
	println("{strconv::format_int(255, 16)} {strconv::quote("a\"b")}")
}
```

## std::math

Floating-point math in the spirit of Go's `math`. Every function is also a method on `f64` (and `f32`): `math::sqrt(x)` is `x.sqrt()`.

| Item | Description |
|---|---|
| constants | `PI`, `TAU`, `E`, `SQRT_2`, `LN_2`, `LN_10`, `LOG2_E`, `LOG10_E`, `INFINITY`, `NEG_INFINITY`, `NAN`, `MAX`, `MIN_POSITIVE`, `EPSILON` |
| rounding | `floor`, `ceil`, `round`, `trunc`, `abs`, `copysign` |
| powers and logs | `sqrt`, `cbrt`, `powi`, `powf`, `exp`, `exp2`, `exp_m1`, `ln`, `log`, `log2`, `log10`, `ln_1p`, `hypot`, `mul_add` |
| trigonometry | `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, and the hyperbolic `sinh` … `atanh` |
| special | `erf`, `erfc`, `gamma`, `ln_gamma`, `fmod`, `ieee_remainder`, `next_after` |
| checks | `is_nan`, `is_infinite`, `is_finite` |

Integer helpers (`checked_add`, `wrapping_mul`, `saturating_sub`, `pow`, `min`, `max`, `abs`, …) are methods on the integer types.

## std::rand

| Item | Description |
|---|---|
| `rand::range(lo, hi)` | a random integer in `lo..hi` |
| `next_u64()`, `next_u32()`, `next_f64()` (in `[0, 1)`), `next_bool()`, `chance(p)` | random values from a shared generator seeded by the OS; safe from any task |
| `shuffle(&mut v)`, `choose(&v)` | shuffle a slice; pick an element (`option<&T>`) |
| `Rng::new()`, `Rng::with_seed(n)` | a fast xoshiro256** generator for one task; a fixed seed repeats exactly. Same methods, plus `normal(mean, std_dev)`, `exponential(rate)`, `fill(&mut buf)` |
| `secure_fill(&mut buf)`, `secure_u64()` | the operating system's cryptographic randomness: use these for keys and tokens, never the others |

```kiln
import std::rand

fun main() {
	die := rand::range(1, 7)
	println("rolled {die}")
	r mut := rand::Rng::with_seed(42)
	println("{r.range(0, 100)} {r.range(0, 100)}")
}
```

## std::errors

`errors::AnyError` holds an error of any type plus context messages, like Go's wrapped errors. A function that can fail in several ways can return `result<T, AnyError>` and use `?` on any error type; `.context(...)` says what was being done.

| Item | Description |
|---|---|
| `errors::new(msg)` | an `AnyError` with just a message |
| `result.context(msg)` | wraps the error with a message (works on any `result` whose error implements `Error`) |
| `AnyError` | displays as the chain, outermost first: `parsing app.toml: line 3: expected '='`; `chain()` lists the messages, `root_cause()` is the innermost |
| `Error` trait | implement it (with `Display` and `Debug`) for your own error types |

Library code with a fixed set of failures should define its own error enum so callers can match on it; `AnyError` is for applications and for passing errors along.

```kiln
import std::errors
import std::errors::AnyError
import std::fs

fun port(path &str) result<u16, AnyError> {
	text := fs::read_to_string(path).context("reading {path}")?
	n := u16::parse(text.trim()).context("parsing {path}")?
	if n == 0 {
		return Err(errors::new("port must not be 0"))
	}
	return Ok(n)
}

fun main() {
	match port("port.txt") {
		Ok(p) => println("port {p}"),
		Err(e) => println("error: {e}"),
	}
}
```

## std::encoding::json

`json::parse(text)` gives a `json::Value` (`Null`, `Bool`, `Int`, `Float`, `String`, `Array`, `Object`); `@derive(Json)` converts your own types.

| Item | Description |
|---|---|
| `json::parse(s)` | `result<Value, json::Error>`; errors say where (`line 3, column 7`) |
| `Value` | `get(key)`, `at(i)`, `pointer("/a/0/b")`, `as_str()`, `as_i64()`, `as_f64()`, `as_bool()`, `as_array()`, `as_object()` (and `_mut`), `kind()`; `encode()` (compact) and `pretty()` |
| `json::Map` | an object: keys keep insertion order (parsing keeps document order); `get`, `get_mut`, `insert`, `remove`, `contains_key`, `iter`, `keys`, `len` |
| `@derive(Json)` | implements `ToJson` and `FromJson`: structs are objects with fields in declaration order; tuple structs are arrays; unit enum variants are strings, other variants `{"Variant": payload}` |
| `json::to_string(&v)`, `to_string_pretty(&v)`, `json::from_str::<T>(s)` | typed encoding and decoding |
| missing fields | an error, except that a missing `option` field reads as `None`; unknown fields are ignored |

```kiln
import std::encoding::json

@derive(Json, Debug)
struct User {
	name string,
	age i64,
	email option<string>,
}

fun main() result<void, json::Error> {
	let u User = json::from_str(r#"{"name": "Ada", "age": 36}"#)?
	println("{u:?}")
	println(json::to_string(&u))
	v := json::parse(r#"{"tags": ["a", "b"]}"#)?
	println("{v.pointer("/tags/1"):?}")
	return Ok()
}
```

## std::encoding::base64 and std::encoding::hex

| Item | Description |
|---|---|
| `base64::encode(bytes)`, `base64::decode(s)` | standard alphabet with padding (RFC 4648); decoding is strict (no whitespace, exact padding) |
| `base64::STANDARD`, `STANDARD_NO_PAD`, `URL_SAFE`, `URL_SAFE_NO_PAD` | engines with `encode`, `decode`, `encoded_len`; `URL_SAFE_NO_PAD` is for URLs and JWTs |
| `hex::encode(bytes)`, `encode_upper`, `hex::decode(s)` | lowercase/uppercase hex; decoding accepts either case |
| `hex::dump(bytes)` | a `hexdump -C` style listing |
| `DecodeError` | says which byte was wrong and where |

```kiln
import std::encoding::base64
import std::encoding::hex

fun main() {
	s := base64::encode(b"hello")
	println("{s} {base64::URL_SAFE_NO_PAD.encode(b"\xfb\xff")}")
	println("{hex::encode(b"hi")} {hex::decode("6869"):?}")
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

Lookups take any borrowed form of the key, so a `HashMap<string, V>` is
searched with a `&str` and no allocation. `HashMap` iteration order is
unspecified; use `BTreeMap` when order matters. Maps and sets implement
`Eq` (independent of order for hash tables), `Clone`, and `Debug`.

```kiln
import std::collections::{BTreeMap, HashMap}

fun main() {
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
