//! Kiln's mid-level IR (MIR): a control-flow graph lowered from the typed AST.
//!
//! MIR makes evaluation order, moves, copies, borrows, and the end of each
//! variable's scope explicit, which is what the borrow checker
//! (`kiln_borrowck`) needs. Later it will also be the input to code generation.
//!
//! Notable lowering decisions:
//! - `defer`red calls are lowered once, in the function's exit block, before
//!   locals are dropped, so they see variables as they are at function exit.
//! - Method call arguments are evaluated before the receiver is auto-borrowed
//!   (two-phase borrows), so `v.push(v.len())` is accepted.
//! - Closures and `spawn` blocks become separate [`Body`]s; their captures are
//!   inferred (moved if the body moves the variable, `&mut` if it mutates it,
//!   `&` otherwise; everything is moved for `move` closures and `spawn move`).
//! - Calls record which arguments the result may borrow from, using the
//!   callee's signature and the lifetime elision rules ([`sig`]).

pub mod dump;
pub mod ir;
mod lower;
pub mod pretty;
pub mod sig;
#[cfg(test)]
mod tests;

pub use ir::*;
pub use lower::lower_fn;

use kiln_typeck::Typed;

/// Lowers every user function (not the prelude) that has a body. Returns
/// `(function index, bodies)`; see [`lower_fn`].
pub fn lower_all(typed: &mut Typed<'_, '_>) -> Vec<(usize, Vec<Body>)> {
    let r = typed.resolved();
    let user: Vec<usize> = (0..r.fns.len()).filter(|i| !r.fns[*i].is_prelude).collect();
    lower_fns(typed, user)
}

/// Lowers the user's functions and the standard library's (not the core
/// prelude's built-ins), for checking the standard library itself.
pub fn lower_with_std(typed: &mut Typed<'_, '_>) -> Vec<(usize, Vec<Body>)> {
    let r = typed.resolved();
    let fns: Vec<usize> = (0..r.fns.len()).filter(|i| !r.fn_is_core(*i)).collect();
    lower_fns(typed, fns)
}

fn lower_fns(typed: &mut Typed<'_, '_>, fns: Vec<usize>) -> Vec<(usize, Vec<Body>)> {
    fns.into_iter()
        .map(|i| (i, lower_fn(typed, i)))
        .filter(|(_, b)| !b.is_empty())
        .collect()
}
