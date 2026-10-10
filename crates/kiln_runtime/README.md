# kiln_runtime

The runtime library linked into every Kiln executable, written in C (`runtime/kiln_rt.c`, `runtime/kiln_sched.c`, and `runtime/kiln_std.c`).

`build.rs` compiles it with the `cc` crate, and `src/lib.rs` embeds the resulting static library (`kiln_runtime::LIB`, named `kiln_runtime::LIB_NAME`) so the `kiln` binary is self-contained; the linker only needs the system C library.

It provides:

- Memory: `kn_alloc`, `kn_free`, `kn_vec_reserve`
- Panics: `kn_panic` flushes stdout, prints `panic: <message>` to stderr, and exits with status 101
- Output: `kn_print` (stdout or stderr, optional newline)
- Strings: building and formatting (`kn_string_push`, `kn_fmt_i64`, `kn_fmt_f64`, ...), comparison, and the `str` methods (`to_upper`, `trim`, `contains`, ...)
- The per-thread `defer` stack used by `DeferDispatch`
- Float helpers (`kn_round`, `kn_powi`)

`kiln_sched.c` is the concurrency runtime, and also provides the C `main`, which starts the scheduler and runs the program's `kn_main` as the first task:

- An M:N work-stealing scheduler: one worker thread per CPU (`KILN_WORKERS=n` overrides), per-worker run queues plus a global queue, and stealing half of a random victim's queue when idle.
- Context switches in hand-written assembly for x86_64 and AArch64 (callee-saved registers only; System V and AAPCS64), and fibers on Windows.
- Task stacks of 1 MiB with a guard page, `mmap`ed and pooled for reuse.
- Tasks (`kn_go`, `kn_task_join`, `kn_task_drop`): the task result lives in a reference-counted handle; a detached task's result is dropped with the type's drop glue.
- Scopes (`kn_scope_new`, `kn_scope_spawn`, `kn_scope_wait`).
- Channels (`kn_chan_*`): a ring buffer plus queues of parked senders and receivers; unbuffered sends hand the value straight to a waiting receiver. Each end is reference-counted; dropping the last sender closes the channel, and the channel and its buffered values are freed with the last end.
- `select` (`kn_select`): locks the involved channels in address order, picks a ready case at random, otherwise parks on every channel and lets the first one to fire claim the task with a compare-and-swap.
- Deadlock detection: when every worker is idle with nothing queued while tasks are parked, the program prints `fatal error: all tasks are asleep - deadlock!` and exits with status 2. Tasks waiting on a timer, a socket, or another external event (`kn_sched_wait_begin`/`kn_sched_wait_end`) are not deadlocked.
- Preemption: tasks also switch at timers, locks, and I/O; compiled loops call `kn_maybe_yield` every 1024 back-edges, which yields if the task has run for 10 ms and other tasks are runnable. The standard library parks tasks through `kn_sched_park`/`kn_sched_ready`.

`kiln_std.c` backs the standard library (`std::time`, `sync`, `io`, `fs`, `os`, `net`):

- Errors: failures return `-(kind << 32 | os_code)`; `kn_os_error_message` renders the OS message (`strerror`, `FormatMessage`, `gai_strerror`). Kinds match `io::ErrorKind`.
- Time: `kn_time_mono`, `kn_time_unix`, and a timer thread with a min-heap; `kn_time_sleep` parks the task.
- Sync: task-aware mutex, rwlock, and wait group (waiters park instead of blocking the worker), and sequentially consistent atomics.
- Network: one poller thread (epoll with one-shot registrations, kqueue, or WSAPoll); sockets are non-blocking, and a task that would block parks until the poller reports readiness. Name lookup uses `getaddrinfo`.
- Files, directories, environment, and processes (`fork`/`execvp` with captured output drained by `poll`; `CreateProcessW` with a helper thread for stderr on Windows). Running a program blocks the calling worker thread until it exits. Windows paths and environment strings convert between UTF-8 and UTF-16.

Layouts shared with the code generator: `String` and `Vec<T>` are `{ ptr, cap, len }`, and `&str` and `&[T]` are `{ ptr, len }`.
