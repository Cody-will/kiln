//! Type checking for Kiln.
//!
//! [`check`] takes a resolved file and reports type errors:
//!
//! - **types and items**: struct/enum field types, generic arity, type aliases;
//! - **traits and impls**: missing or extra trait methods, signature mismatches,
//!   trait bounds on generic calls (`T: Ord`), `#[derive(...)]`;
//! - **inference**: Hindley–Milner-style unification within function bodies,
//!   with integer/float literal defaulting (`i32`/`f64`);
//! - **errors**: `Result`/`Option` with `?`, including `From` conversion of error types;
//! - **explicit returns** (SPEC §3.1): non-unit functions must `return` on every
//!   path; a trailing expression is not an implicit return;
//! - **value-producing `if`/`match`** (SPEC §3.2) and match exhaustiveness for enums and `bool`;
//! - **mutability**: assignment to immutable bindings, `&mut` borrows, `&mut self` calls;
//! - **channels**: `tx <- v`, `<-rx`, `select`, `chan::<T>(n)`.
//!
//! Lifetimes and borrow checking are later phases (see docs/ROADMAP.md).

mod check;
mod collect;
mod display;
#[cfg(test)]
mod tests;
pub mod ty;

use kiln_lexer::Diagnostic;
use kiln_parser::ast::File;
use kiln_resolve::Resolved;

/// Type-checks a resolved file and returns all diagnostics.
pub fn check(resolved: &Resolved<'_>) -> Vec<Diagnostic> {
    let mut c = collect::Checker::new(resolved);
    c.collect();
    c.check_bodies();
    c.errors
}

/// Convenience: parses, resolves, and type-checks `src`, returning every
/// diagnostic (parse, resolve, then type errors). Type checking is skipped
/// when earlier phases fail.
pub fn check_source(src: &str) -> Vec<Diagnostic> {
    let parsed = kiln_parser::parse(src);
    if !parsed.errors.is_empty() {
        return parsed.errors;
    }
    let prelude: File = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    if !resolved.errors.is_empty() {
        return resolved.errors;
    }
    check(&resolved)
}
