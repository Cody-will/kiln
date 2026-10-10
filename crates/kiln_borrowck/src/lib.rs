//! Borrow checking for Kiln.
//!
//! Runs on MIR ([`kiln_mir`]) after type checking and enforces Kiln's
//! ownership rules (SPEC §6): moves vs `Copy`, use after move (including
//! partial moves and moves in loops), initialization, shared vs mutable borrow
//! conflicts with non-lexical lifetimes, borrows outliving their owners (and
//! `Drop` order), returning references to locals, lifetime elision and explicit
//! `'a` annotations, `defer`, closure captures, `'static` + `Send` for `go`,
//! and `Sync`/`Send` for scoped tasks. See `body.rs` for the algorithm.

mod body;
mod sigcheck;
#[cfg(test)]
mod tests;

use kiln_lexer::Diagnostic;
use kiln_typeck::Typed;

/// Borrow-checks every user function. Assumes type checking succeeded.
pub fn check(typed: &mut Typed<'_, '_>) -> Vec<Diagnostic> {
    let r = typed.resolved();
    let mut errors = vec![];
    sigcheck::check_items(r, &mut errors);
    for f in r.fns.iter().filter(|f| !f.is_prelude) {
        let (outer, outer_types) = match (f.impl_idx, f.trait_def) {
            (Some(idx), _) => (
                r.impls[idx].decl.generics.clone(),
                kiln_mir::sig::type_param_names(&r.impls[idx].decl.generics),
            ),
            _ => (Default::default(), vec![]),
        };
        sigcheck::check_fn(f.decl, &outer, &outer_types, r, &mut errors);
    }
    if !errors.is_empty() {
        return errors;
    }
    for (i, bodies) in kiln_mir::lower_all(typed) {
        let bounds = typed
            .fn_results(i)
            .map(|res| res.bounds.clone())
            .unwrap_or_default();
        for b in &bodies {
            let mut ck = body::Ck::new(b, typed, bounds.clone());
            ck.run();
            errors.extend(ck.errors);
        }
    }
    errors.sort_by_key(|d| d.span.start);
    errors
}

/// Runs the whole front end on `src`: parse, resolve, type-check, and
/// borrow-check, stopping after the first phase that reports errors.
pub fn check_source(src: &str) -> Vec<Diagnostic> {
    let parsed = kiln_parser::parse(src);
    if !parsed.errors.is_empty() {
        return parsed.errors;
    }
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    if !resolved.errors.is_empty() {
        return resolved.errors;
    }
    let mut typed = kiln_typeck::typeck(&resolved);
    if !typed.errors().is_empty() {
        return typed.errors().to_vec();
    }
    check(&mut typed)
}
