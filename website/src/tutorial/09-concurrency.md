# Concurrency: `go`, channels, `select`

Kiln's concurrency model comes from Go: lightweight **tasks** that communicate over **channels**. Kiln adds compile-time safety from ownership: a value sent on a channel is *moved*, so two tasks can never race on the same data.

Everything in this chapter compiles and runs. Tasks run on an M:N scheduler: many tasks share a few OS threads (one per CPU core by default; set `KILN_WORKERS` to change it), and idle threads steal work from busy ones.

## Starting tasks with `go`

```kiln
fn say(msg: &str) {
	println(msg)
}

fn square(x: i64) -> i64 {
	return x * x
}

fn main() {
	a := go say("from a task")
	b := go move {
		println("from a block")
	}
	say("from main")
	a.join()
	b.join()

	t := go square(7)
	match t.join() {
		Ok(v) => println("square: {v}"),
		Err(_) => println("the task panicked"),
	}
}
```

`go f(args)` runs a call in a new task, and `go move { ... }` runs a block. Tasks are cheap (each has its own small stack); you can start thousands.

`go` is an expression: it returns a `Task<T>` handle, where `T` is what the call or block returns. `t.join()` waits for the task and gives back `Result<T, Panic>`. Like Go, a program ends when `main` returns, even if other tasks are still running, so join the tasks you need to finish. Dropping a `Task` without joining detaches it.

<div class="status">
🚧 A panic in any task currently ends the whole program (exit status 101), so <code>join</code> never returns <code>Err</code> yet.
</div>

A task may keep running after the function that started it returns, so it must **own** everything it uses:

- `go f(args)` moves the arguments into the task. Passing a reference to a local (`go f(&data)`) is an error; string literals (`&'static str`) are fine.
- A `go` block that uses local variables must be `go move { ... }`, which moves them in. Clone first if you still need a value afterwards.
- Everything moved into a task must be `Send`. Structs and enums are `Send` automatically when all their fields are; raw pointers are not.

```text
error: argument to `go` borrows `data`, which may not live as long as the task
 --> main.kn:4:2
  |
4 | 	go report(&data)
  | 	^^^^^^^^^^^^^^^^
  = help: tasks started with `go` may outlive the current function: pass an owned value (e.g. `.clone()`), or use `scope(|s| { s.spawn(...) })` for tasks that borrow
```

### Scoped tasks

When tasks only need to *borrow* data, start them inside `scope`. Every task spawned with `s.spawn` is joined before `scope` returns, so borrowing locals is safe. Shared borrows require the type to be `Sync`:

```kiln
fn main() {
	data mut := Vec::new()
	for i in 0..10 {
		data.push(i)
	}
	hits mut := 0
	scope(|s| {
		s.spawn(|| {
			sum mut := 0
			for x in &data {
				sum += *x
			}
			println("sum {sum}")
		})
		s.spawn(|| {
			hits += 1           // one task may borrow mutably
		})
	})
	println("hits {hits}, {data.len()} items")
}
```

The borrow checker treats the spawned closures like any other borrows: two tasks can share `data`, but they can't both mutate `hits`.

## Channels

`chan::<T>(capacity)` creates a channel and returns its two ends: a **send-only** end `chan<- T` and a **receive-only** end `<-chan T`. A capacity of `0` makes an unbuffered channel.

```kiln
fn main() {
	(tx, rx) := chan::<i32>(10)
	tx <- 1          // send
	tx <- 2
	v := <-rx        // receive (blocks until a value arrives)
	println("{v}")
}
```

The direction is part of the type, so mistakes are caught at compile time:

```text
error: cannot send on a value of type `<-chan i32`
  = help: sending requires a send-only channel `chan<- T`
```

Both ends can be cloned, so many tasks can send to or receive from the same channel. A `for` loop receives until the channel is closed, and the channel closes automatically when its last sender is dropped:

```kiln
fn main() {
	(tx, rx) := chan::<i32>(0)
	go move {
		for i in 0..5 {
			tx <- i
		}
	}                           // the task ends and drops `tx`, closing the channel
	for v in rx {
		println("got {v}")
	}
}
```

On an unbuffered channel, a send waits until another task receives (and the other way round). Receiving with `<-rx` from a closed, empty channel panics; use `rx.recv()` when closing is expected. Values still buffered in a channel when it's dropped are dropped with it.

The element type must be `Send`, since its values move to another task.

If every task is blocked on a channel and nothing can wake them, the program stops with `fatal error: all tasks are asleep - deadlock!` (exit status 2), like Go.

## A worker pool

```kiln
struct Job {
	id: u32,
}

fn worker(id: u32, jobs: <-chan Job, results: chan<- String) {
	for job in jobs {           // ends when `jobs` is closed
		results <- "worker {id} did job {job.id}"
	}
}

fn main() {
	(job_tx, job_rx) := chan::<Job>(100)
	(res_tx, res_rx) := chan::<String>(100)

	for id in 0..4 {
		go worker(id, job_rx.clone(), res_tx.clone())
	}
	for i in 0..10 {
		job_tx <- Job { id: i }
	}
	drop(job_tx)                // dropping the last sender closes the channel

	for i in 0..10 {
		println(<-res_rx)
	}
}
```

## `select`

`select` waits on several channel operations at once and runs the first one that's ready:

```kiln
fn main() {
	(data_tx, data_rx) := chan::<String>(0)
	(quit_tx, quit_rx) := chan::<bool>(1)
	go move {
		for i in 0..3 {
			data_tx <- "message {i}"
		}
		quit_tx <- true
	}
	loop {
		select {
			msg := <-data_rx => println("got {msg}"),
			<-quit_rx => {
				println("quitting")
				break
			},
		}
	}
}
```

Arms can receive (`v := <-rx`, or `<-rx` to discard), send (`tx <- v`), or be a default arm (`_ => ...`) that runs when nothing is ready:

```kiln
fn main() {
	(tx, rx) := chan::<i32>(1)
	select {
		v := <-rx => println("got {v}"),
		_ => println("nothing yet"),
	}
	select {
		tx <- 42 => println("sent"),
		_ => println("buffer full"),
	}
	println("{<-rx}")
}
```

When several arms are ready, `select` picks one at random, so no channel can starve the others.

## Non-blocking operations

| Method | Meaning |
|---|---|
| `tx.try_send(v)` | send without blocking; `Err(TrySendError::Full(v))` or `Err(TrySendError::Closed(v))` hands the value back |
| `rx.try_recv()` | receive without blocking; `Err(TryRecvError::Empty)` or `Err(TryRecvError::Closed)` |
| `rx.recv()` | blocking receive; `None` once the channel is closed |
| `tx.close()` | close explicitly (dropping every sender also closes) |
| `rx.len()`, `rx.cap()` | number of queued items, buffer capacity |

## Sharing state

Prefer channels. When tasks really must share data, use `Arc<Mutex<T>>` from `std::sync` (🚧 the standard library is not implemented yet).
