# kiln_borrowck

Ownership and borrow checking for Kiln, on top of `kiln_mir`. Run it with
`kiln check <file>` (after parsing, name resolution, and type checking).

What it checks:

| Rule | Example diagnostic |
|---|---|
| Use after move (incl. partial moves, moves in loops, moves into closures and tasks) | ``use of moved value: `s` `` + note "value moved here" |
| Initialization (`let x: T` before assignment) | ``used binding `x` isn't initialized`` |
| Moves out of references, indexes, and `Drop` types | ``cannot move out of `*p.a`, which is behind a shared reference`` |
| Shared vs mutable borrows, with non-lexical lifetimes | ``cannot borrow `v` as mutable because it is also borrowed as immutable`` |
| Writes and moves while borrowed | ``cannot assign to `n` because it is borrowed`` |
| Borrows outliving their owner (and `Drop` order) | ``` `x` does not live long enough``` + "dropped here" / "borrow later used here" |
| Returning references to locals | ``cannot return reference to local variable `x` `` |
| Lifetime elision and explicit `'a` | `missing lifetime specifier`, `lifetime may not live long enough` |
| `defer` sees variables at function exit | ``borrow of moved value: `s` `` |
| `spawn` needs `'static` + `Send`; `spawn` blocks need `move` | ``argument to `spawn` borrows `s` ...``, ``` `Handle` cannot be sent between tasks safely``` |
| Scoped tasks may borrow; shared captures must be `Sync` | ``` `Handle` cannot be shared between tasks safely``` |

How it works (details in `src/body.rs`): a forward dataflow analysis of
moved/uninitialized paths; a backward liveness analysis; and loans created by
each borrow that flow into the locals holding them. A loan is live while a
holder is live, which gives non-lexical lifetimes. `src/sigcheck.rs` checks
signatures and struct definitions.

Known gaps are listed in `docs/ARCHITECTURE.md`.
