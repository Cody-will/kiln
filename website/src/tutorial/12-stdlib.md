# Files, processes, and the network

This chapter puts the standard library to work. We'll build a small guest book: a web service that keeps messages in memory, saves them to a file, and answers over HTTP. Along the way we'll use `std::fs`, `std::os`, `std::log`, `std::sync`, and `std::net::http`. The [standard library reference](../stdlib.md) lists everything that is available.

## Reading and writing files

`fs::read_to_string` reads a whole file. Like everything that can fail, it returns a `result`, so `?` hands errors to the caller:

```kiln
import std::fs
import std::io

fun load(file &str) result<Vec<string>, io::Error> {
	if !fs::exists(file) {
		return Ok(Vec::new())
	}
	text := fs::read_to_string(file)?
	entries mut := Vec::new()
	for line in text.lines() {
		entries.push(line.to_string())
	}
	return Ok(entries)
}

fun save(file &str, entries &Vec<string>) result<void, io::Error> {
	text mut := string::new()
	for e in entries {
		text.push("{e}\n")
	}
	return fs::write_string(file, &text)
}

fun main() result<void, io::Error> {
	entries mut := load("guestbook.txt")?
	entries.push("hello from the tutorial".to_string())
	save("guestbook.txt", &entries)?
	println("{entries.len()} entries")
	return Ok()
}
```

When something goes wrong, the error says what and where. If the file exists but can't be read, `main` returns the error, and the program prints something like `open guestbook.txt: Permission denied` and exits with status 1. To react to a particular failure, match on its kind:

```kiln
import std::fs
import std::io

fun main() {
	match fs::read_to_string("settings.toml") {
		Ok(text) => println("{text.len()} bytes of settings"),
		Err(e) => {
			if e.kind() == io::ErrorKind::NotFound {
				println("no settings file; using defaults")
			} else {
				eprintln("error: {e}")
			}
		},
	}
}
```

## Configuration from the environment

`os::getenv` returns an `option<string>`; `unwrap_or` supplies a default. `os::args()` holds the command line.

```kiln
import std::os

fun main() {
	addr := os::getenv("ADDR").unwrap_or(":8080".to_string())
	file := match os::args().len() {
		0 | 1 => "guestbook.txt".to_string(),
		_ => os::args()[1].clone(),
	}
	println("serving {file} on {addr}")
}
```

## The web service

An HTTP handler is a function, or a closure, that takes a `Request` and returns a `Response`. The router sends each request to the handler for its path. Every connection runs in its own task, so handlers share state the way any tasks do: through an `Arc`, with a `Mutex` around data that changes.

```kiln
import std::fs
import std::io
import std::log
import std::net::http
import std::os
import std::sync::{Arc, Mutex}

fun save(file &str, entries &Vec<string>) result<void, io::Error> {
	text mut := string::new()
	for e in entries {
		text.push("{e}\n")
	}
	return fs::write_string(file, &text)
}

fun main() result<void, io::Error> {
	file := "guestbook.txt"
	book := Arc::new(Mutex::new(Vec::new()))
	router mut := http::Router::new()

	// GET /: list the entries.
	b := book.clone()
	router.get("/", move |req http::Request| {
		entries := b.lock()
		page mut := string::new()
		for e in &*entries {
			page.push("- {e}\n")
		}
		return http::Response::text(200, &page)
	})

	// POST /sign with the message as the body.
	b2 := book.clone()
	router.post("/sign", move |req http::Request| {
		msg := req.body_text()
		if msg.trim().is_empty() {
			return http::Response::text(400, "empty message\n")
		}
		entries mut := b2.lock()
		entries.push(msg.trim().to_string())
		if let Err(e) = save(file, &*entries) {
			log::error("saving: {e}")
			return http::Response::text(500, "could not save\n")
		}
		log::info("new entry ({entries.len()} total)")
		return http::Response::text(201, "thanks!\n")
	})

	addr := os::getenv("ADDR").unwrap_or("127.0.0.1:8080".to_string())
	log::info("listening on {addr}")
	return router.listen(&addr)
}
```

Run it and talk to it with `curl`:

```text
$ kiln run guestbook.kn
2026-10-10T15:24:05Z INFO listening on 127.0.0.1:8080

$ curl -d "hello kiln" localhost:8080/sign
thanks!
$ curl localhost:8080/
- hello kiln
```

The lock guard (`entries`) unlocks when it goes out of scope at the end of the handler. `&*entries` borrows the `Vec` inside the guard. 🚧 Kiln will soon do this conversion for you where a `&Vec<string>` is expected, as Rust does. While one task holds the lock, others that want it park; their threads go on running other tasks.

## Calling other services

The same module has a client. `http::get` returns the response, whatever its status; check `status` yourself:

```kiln
import std::io
import std::net::http

fun main() result<void, io::Error> {
	resp := http::get("http://127.0.0.1:8080/")?
	if resp.status != 200 {
		eprintln("server said {resp.status}")
		return Ok()
	}
	print(resp.body_text())
	return Ok()
}
```

## Running other programs

`os::Command` runs a program and waits for it. `output()` captures what it prints:

```kiln
import std::io
import std::os

fun main() result<void, io::Error> {
	out := os::Command::new("git").args(&["log", "--oneline", "-3"]).output()?
	if !out.success() {
		eprintln("git failed: {out.stderr_text()}")
		return Ok()
	}
	print(out.stdout_text())
	return Ok()
}
```

If the program doesn't exist, `output()` returns an error whose kind is `io::ErrorKind::NotFound`.

## Lower-level networking

Under the HTTP layer are TCP and UDP sockets. `listener.incoming()` is a channel of connections, so a server is a `for` loop that starts a task per connection:

```kiln
import std::io
import std::io::BufReader
import std::net::{TcpListener, TcpStream}

fun shout(conn TcpStream) result<void, io::Error> {
	r mut := BufReader::new(&conn)
	while let Some(line) = r.next_line()? {
		conn.write_str("{line.to_upper()}\n")?
	}
	return Ok()
}

fun main() result<void, io::Error> {
	listener := TcpListener::bind("127.0.0.1:7000")?
	for conn in listener.incoming() {
		c := conn?
		spawn move {
			if let Err(e) = shout(c) {
				eprintln("connection: {e}")
			}
		}
	}
	return Ok()
}
```

`BufReader::new(&conn)` borrows the connection for reading, and `conn.write_str` writes through the same shared reference. Sockets allow one reader and one writer at a time, so you can also hand `&conn` to two tasks in a `scope`.
