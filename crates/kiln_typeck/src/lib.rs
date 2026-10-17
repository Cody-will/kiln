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
pub mod dump;
mod ffi;
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
    c.check_ffi();
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
            Ty::Fn(_, _, is_mut) => !is_mut,
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

    /// Declared generic parameter names of function `i`: `(its own, enclosing impl/trait's)`.
    pub fn fn_generics(&self, i: usize) -> (Vec<String>, Vec<String>) {
        let s = &self.c.sigs[i];
        (s.generics.clone(), s.outer_generics.clone())
    }

    /// Methods of trait `tr` (indices into `Resolved::fns`), in declaration order.
    pub fn trait_methods(&self, tr: DefId) -> Vec<usize> {
        let r = self.c.r;
        (0..r.fns.len())
            .filter(|i| r.fns[*i].trait_def == Some(tr))
            .collect()
    }

    /// The methods in a `dyn principal` vtable, in slot order: the trait's own
    /// methods, then each supertrait's (transitively, without repeats).
    pub fn dyn_methods(&self, principal: DefId) -> Vec<usize> {
        let mut traits = vec![principal];
        let mut i = 0;
        while i < traits.len() {
            if let Some(info) = self.c.traits.get(&traits[i]) {
                for s in &info.supertraits {
                    if !traits.contains(s) {
                        traits.push(*s);
                    }
                }
            }
            i += 1;
        }
        traits.iter().flat_map(|t| self.trait_methods(*t)).collect()
    }

    /// Whether function `i` has a body.
    pub fn has_body(&self, i: usize) -> bool {
        self.c.sigs[i].has_body
    }

    /// For a type with a user `Deref` (or, when `mutable`, `DerefMut`) impl:
    /// the target type and the trait method (`deref` / `deref_mut`) to call.
    pub fn deref_target(&mut self, t: &Ty, mutable: bool) -> Option<(Ty, usize)> {
        let r = self.c.r;
        let Ty::Adt(d, _) = t else { return None };
        if ["Box", "string", "Vec"]
            .iter()
            .any(|n| r.prelude_type(n) == Some(*d))
        {
            return None;
        }
        let (tn, mn) = if mutable {
            ("DerefMut", "deref_mut")
        } else {
            ("Deref", "deref")
        };
        let tr = r.prelude_type(tn)?;
        let tf = (0..r.fns.len())
            .find(|&i| r.fns[i].trait_def == Some(tr) && r.fns[i].decl.name.name == mn)?;
        for imp in self.c.impls.clone() {
            let Some((itr, iargs)) = &imp.trait_ref else {
                continue;
            };
            if *itr != tr {
                continue;
            }
            let snap = self.c.infer.snapshot();
            let map: std::collections::HashMap<String, Ty> = imp
                .generics
                .iter()
                .map(|g| (g.clone(), self.c.infer.fresh(ty::VarKind::General)))
                .collect();
            if self.c.infer.unify(&imp.self_ty.subst(&map), t) {
                let target = iargs.first().map(|a| self.c.infer.zonk(&a.subst(&map)));
                self.c.infer.restore(snap);
                return target.map(|x| (x, tf));
            }
            self.c.infer.restore(snap);
        }
        None
    }

    /// Resolves a call of trait method `trait_fn` (an index into `Resolved::fns`)
    /// for the concrete types in `substs` (which must contain `Self` and the
    /// trait's generic parameters). Returns the implementing function and its
    /// substitution, or the trait's default method.
    pub fn resolve_trait_method(
        &mut self,
        trait_fn: usize,
        substs: &std::collections::HashMap<String, Ty>,
    ) -> Option<(usize, std::collections::HashMap<String, Ty>)> {
        use std::collections::HashMap;
        let r = self.c.r;
        let tr = r.fns[trait_fn].trait_def?;
        let name = r.fns[trait_fn].decl.name.name.clone();
        let self_ty = substs.get("Self")?.clone();
        let tgen = self
            .c
            .traits
            .get(&tr)
            .map(|t| t.generics.clone())
            .unwrap_or_default();
        let targs: Vec<Ty> = tgen
            .iter()
            .map(|g| substs.get(g).cloned().unwrap_or(Ty::Unknown))
            .collect();
        let (own_generics, _) = self.fn_generics(trait_fn);
        for imp in self.c.impls.clone() {
            let Some((itr, iargs)) = &imp.trait_ref else {
                continue;
            };
            if *itr != tr {
                continue;
            }
            let snap = self.c.infer.snapshot();
            let map: HashMap<String, Ty> = imp
                .generics
                .iter()
                .map(|g| (g.clone(), self.c.infer.fresh(ty::VarKind::General)))
                .collect();
            let ok = self.c.infer.unify(&imp.self_ty.subst(&map), &self_ty)
                && iargs.iter().zip(&targs).all(|(a, b)| {
                    matches!(b, Ty::Unknown) || self.c.infer.unify(&a.subst(&map), b)
                });
            if ok {
                let mut out: HashMap<String, Ty> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), self.c.infer.zonk(v)))
                    .collect();
                self.c.infer.restore(snap);
                let Some(&mi) = imp.methods.get(&name) else {
                    // The impl uses the trait's default method.
                    return self.has_body(trait_fn).then(|| (trait_fn, substs.clone()));
                };
                let (impl_own, _) = self.fn_generics(mi);
                for (a, b) in own_generics.iter().zip(impl_own) {
                    if let Some(t) = substs.get(a) {
                        out.insert(b, t.clone());
                    }
                }
                return Some((mi, out));
            }
            self.c.infer.restore(snap);
        }
        self.has_body(trait_fn).then(|| (trait_fn, substs.clone()))
    }

    /// The type of field `f` of `t` (`name`, `0`, or `Variant.name` for
    /// enum payloads), or `Unknown`.
    pub fn field_ty(&self, t: &Ty, f: &str) -> Ty {
        match t {
            Ty::Tuple(ts) => f
                .parse::<usize>()
                .ok()
                .and_then(|i| ts.get(i).cloned())
                .unwrap_or(Ty::Unknown),
            Ty::Adt(d, _) => {
                let (variant, name) = match f.split_once('.') {
                    Some((v, n)) => (Some(v), n),
                    None => (None, f),
                };
                let vdef = variant.and_then(|v| {
                    self.c
                        .r
                        .variants(*d)
                        .into_iter()
                        .find(|x| self.c.r.def(*x).name == v)
                });
                self.adt_fields(t, vdef)
                    .into_iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, t)| t)
                    .unwrap_or(Ty::Unknown)
            }
            _ => Ty::Unknown,
        }
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
