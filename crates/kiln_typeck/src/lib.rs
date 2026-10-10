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

pub use check::{FnResults, MethodInfo};
use kiln_resolve::DefId;
use ty::Ty;

/// Type-checks a resolved file and returns all diagnostics.
pub fn check(resolved: &Resolved<'_>) -> Vec<Diagnostic> {
    typeck(resolved).errors().to_vec()
}

/// Type-checks a resolved file, keeping the results for later phases.
pub fn typeck<'r, 'a>(resolved: &'r Resolved<'a>) -> Typed<'r, 'a> {
    let mut c = collect::Checker::new(resolved);
    c.collect();
    c.check_bodies();
    Typed { c }
}

/// The output of type checking: diagnostics, per-function types, and trait queries.
pub struct Typed<'r, 'a> {
    c: collect::Checker<'r, 'a>,
}

impl<'r, 'a> Typed<'r, 'a> {
    /// Type errors.
    pub fn errors(&self) -> &[Diagnostic] {
        &self.c.errors
    }

    /// The resolver output this was computed from.
    pub fn resolved(&self) -> &'r Resolved<'a> {
        self.c.r
    }

    /// Type results for function `i` (an index into `Resolved::fns`), if it has a body.
    pub fn fn_results(&self, i: usize) -> Option<&FnResults> {
        self.c.results.get(i).and_then(|r| r.as_ref())
    }

    /// The declared parameter types of function `i` (excluding `self`) and its return type.
    pub fn signature(&self, i: usize) -> (Vec<Ty>, Ty) {
        let s = &self.c.sigs[i];
        (s.params.clone(), s.ret.clone())
    }

    /// Whether `t` implements the prelude trait `name` (e.g. `"Copy"`, `"Send"`),
    /// with `bounds` as the generic bounds in scope.
    pub fn implements(&mut self, t: &Ty, name: &str, bounds: &[(String, DefId)]) -> bool {
        let Some(tr) = self.c.r.prelude_type(name) else {
            return false;
        };
        check::Body::detached(&mut self.c, bounds.to_vec()).implements(t, tr)
    }

    /// Whether values of type `t` are copied rather than moved.
    pub fn is_copy(&mut self, t: &Ty, bounds: &[(String, DefId)]) -> bool {
        match t {
            Ty::Unknown | Ty::Never | Ty::Var(_) => true,
            Ty::Fn(..) => true,
            _ => self.implements(t, "Copy", bounds),
        }
    }

    /// Whether `t` has an explicit `impl Drop`.
    pub fn has_drop_impl(&mut self, t: &Ty) -> bool {
        let Some(tr) = self.c.r.prelude_type("Drop") else {
            return false;
        };
        matches!(t, Ty::Adt(..)) && check::Body::detached(&mut self.c, vec![]).has_impl_pub(t, tr)
    }

    /// Renders a type for diagnostics.
    pub fn fmt_ty(&self, t: &Ty) -> String {
        self.c.fmt_ty(t)
    }

    /// Field types of a struct type, or of enum variant `variant` of an enum type.
    pub fn adt_fields(&self, t: &Ty, variant: Option<DefId>) -> Vec<(String, Ty)> {
        let Ty::Adt(d, args) = t else { return vec![] };
        let Some(info) = self.c.adts.get(d) else {
            return vec![];
        };
        let map = info
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        let shape = match variant {
            None => &info.shape,
            Some(v) => match info.variants.iter().find(|(id, _)| *id == v) {
                Some((_, sh)) => sh,
                None => return vec![],
            },
        };
        match shape {
            collect::Shape::Named(fs) => {
                fs.iter().map(|(n, t)| (n.clone(), t.subst(&map))).collect()
            }
            collect::Shape::Tuple(ts) => ts
                .iter()
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.subst(&map)))
                .collect(),
            collect::Shape::Unit => vec![],
        }
    }

    /// Whether struct/enum `d` declares lifetime parameters (so values may hold borrows).
    pub fn adt_has_lifetimes(&self, d: DefId) -> bool {
        use kiln_parser::ast::{GenericParam, ItemKind};
        let generics = match self.c.r.def(d).item.map(|i| &i.kind) {
            Some(ItemKind::Struct(s)) => &s.generics,
            Some(ItemKind::Enum(e)) => &e.generics,
            _ => return false,
        };
        generics
            .params
            .iter()
            .any(|p| matches!(p, GenericParam::Lifetime { .. }))
    }

    /// Field types of a struct type (`(name or index, type)`), substituted.
    pub fn struct_fields(&self, t: &Ty) -> Vec<(String, Ty)> {
        let Ty::Adt(d, args) = t else { return vec![] };
        let Some(info) = self.c.adts.get(d) else {
            return vec![];
        };
        let map = info
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        match &info.shape {
            collect::Shape::Named(fs) => {
                fs.iter().map(|(n, t)| (n.clone(), t.subst(&map))).collect()
            }
            collect::Shape::Tuple(ts) => ts
                .iter()
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.subst(&map)))
                .collect(),
            collect::Shape::Unit => vec![],
        }
    }
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
