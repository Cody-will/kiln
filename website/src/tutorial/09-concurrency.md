# Concurrency: `go`, channels, `select`

Kiln's concurrency model comes from Go: lightweight **tasks** that communicate over **channels**. Kiln adds compile-time safety from ownership: a value sent on a channel is *moved*, so two tasks can never race on the same data.

<div class="status">
🚧 <strong>The runtime does not exist yet.</strong> Everything in this chapter is parsed and type-checked today (channel element types, channel direction, <code>select</code> arms), but programs can't run until the code generator and scheduler land (milestone M4). <code>Send</code>/<code>Sync</code> checks on what crosses tasks are also part of M4.
</div>

## Starting tasks with `go`

```kiln
fn say(msg: &str) {
	println(msg)
}

fn main() {
	go say("from a task")
	go {
		println("from a block")
	}
	say("from main")
}
```

`go f(args)` runs a call in a new task, and `go { ... }` (or `go move { ... }`) runs a block. Tasks are cheap; you can start thousands.

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

Both ends can be cloned, so many tasks can send to or receive from the same channel. A `for` loop receives until the channel is closed:

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
	(data_tx, data_rx) := chan::<String>(1)
	(quit_tx, quit_rx) := chan::<bool>(1)
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

Arms can receive (`v := <-rx`, or `<-rx` to discard), send (`tx <- v`), or be a `default` arm that runs when nothing is ready.

## Non-blocking operations

| Method | Meaning |
|---|---|
| `tx.try_send(v)` | send without blocking; `Err` if the buffer is full |
| `rx.try_recv()` | receive without blocking |
| `rx.recv()` | blocking receive; `None` once the channel is closed |
| `tx.close()` | close explicitly (dropping every sender also closes) |
| `rx.len()`, `rx.cap()` | number of queued items, buffer capacity |

## Sharing state

Prefer channels. When tasks really must share data, use `Arc<Mutex<T>>` from `std::sync` (🚧 the standard library is not implemented yet).
