//! Lifetime information from function signatures: which parameters a returned
//! reference may borrow from (with the spec's elision rules).

use kiln_parser::ast::{
    Bound, FnDecl, GenericArg, GenericParam, Generics, SelfParam, Type, TypeKind,
};
use kiln_resolve::{DefKind, Res, Resolved};
use std::collections::BTreeSet;

/// Borrow-related facts about one type in a signature.
#[derive(Debug, Default, Clone)]
pub struct TyLifetimes {
    /// Named lifetimes, without the quote (`'static` is included as `static`).
    pub named: BTreeSet<String>,
    /// Whether the type contains a reference (or lifetime-carrying struct) with an elided lifetime.
    pub elided: bool,
    /// Whether the type mentions a generic parameter, `Self`, a trait object, or a function type.
    pub generic: bool,
}

impl TyLifetimes {
    /// Whether values of this type may hold borrows.
    pub fn has_refs(&self) -> bool {
        self.elided || self.generic || self.has_lifetimes()
    }

    /// Whether the type has a (non-`'static`) lifetime, elided or named.
    /// This is what lifetime elision counts.
    pub fn has_lifetimes(&self) -> bool {
        self.elided || self.named.iter().any(|n| n != "static")
    }
}

/// Collects lifetime facts for `t`. `generics` are the type parameter names in scope.
pub fn ty_lifetimes(t: &Type, generics: &[String], r: &Resolved<'_>) -> TyLifetimes {
    let mut out = TyLifetimes::default();
    walk(t, generics, r, &mut out);
    out
}

fn walk(t: &Type, generics: &[String], r: &Resolved<'_>, out: &mut TyLifetimes) {
    match &t.kind {
        TypeKind::Ref {
            lifetime, inner, ..
        } => {
            match lifetime {
                Some(l) => {
                    out.named.insert(l.name.clone());
                }
                None => out.elided = true,
            }
            walk(inner, generics, r, out);
        }
        TypeKind::Path(p) => {
            if p.segments.len() == 1 {
                let n = &p.segments[0].ident.name;
                if generics.contains(n) || n == "Self" {
                    out.generic = true;
                }
            }
            let last = p.segments.last().unwrap();
            let mut has_lt_arg = false;
            for a in &last.args {
                match a {
                    GenericArg::Lifetime(l) => {
                        has_lt_arg = true;
                        out.named.insert(l.name.clone());
                    }
                    GenericArg::Type(t) | GenericArg::Binding { ty: t, .. } => {
                        walk(t, generics, r, out)
                    }
                }
            }
            if !has_lt_arg {
                if let Res::Def(d) = r.path(p.span) {
                    if matches!(r.def(d).kind, DefKind::Struct | DefKind::Enum)
                        && adt_lifetime_params(r, d) > 0
                    {
                        out.elided = true;
                    }
                }
            }
            if matches!(r.path(p.span), Res::TyParam(_) | Res::SelfTy) {
                out.generic = true;
            }
        }
        TypeKind::Ptr { inner, .. } | TypeKind::Slice(inner) | TypeKind::Array(inner, _) => {
            walk(inner, generics, r, out)
        }
        TypeKind::ChanSend(inner) | TypeKind::ChanRecv(inner) => walk(inner, generics, r, out),
        TypeKind::Tuple(ts) => ts.iter().for_each(|x| walk(x, generics, r, out)),
        TypeKind::Fn { params, ret } => {
            out.generic = true;
            params.iter().for_each(|x| walk(x, generics, r, out));
            if let Some(x) = ret {
                walk(x, generics, r, out);
            }
        }
        TypeKind::Dyn(bounds) | TypeKind::Impl(bounds) => {
            out.generic = true;
            for b in bounds {
                if let Bound::Lifetime(l) = b {
                    out.named.insert(l.name.clone());
                }
            }
        }
        TypeKind::Never | TypeKind::Infer => {}
    }
}

/// Number of lifetime parameters declared by struct/enum `d`.
pub fn adt_lifetime_params(r: &Resolved<'_>, d: kiln_resolve::DefId) -> usize {
    use kiln_parser::ast::ItemKind;
    let g = match r.def(d).item.map(|i| &i.kind) {
        Some(ItemKind::Struct(s)) => &s.generics,
        Some(ItemKind::Enum(e)) => &e.generics,
        _ => return 0,
    };
    g.params
        .iter()
        .filter(|p| matches!(p, GenericParam::Lifetime { .. }))
        .count()
}

/// Type parameter names declared in `g`.
pub fn type_param_names(g: &Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect()
}

/// Lifetime facts for each parameter of `decl` (`self` first, if present) and for its return type.
pub fn fn_lifetimes(
    decl: &FnDecl,
    outer: &[String],
    r: &Resolved<'_>,
) -> (Vec<TyLifetimes>, TyLifetimes) {
    let mut generics: Vec<String> = outer.to_vec();
    generics.extend(type_param_names(&decl.generics));
    let mut params = Vec::new();
    if let Some(sp) = &decl.self_param {
        let mut l = TyLifetimes {
            generic: true,
            ..Default::default()
        };
        if let SelfParam::Ref { lifetime, .. } = sp {
            match lifetime {
                Some(n) => {
                    l.named.insert(n.name.clone());
                }
                None => l.elided = true,
            }
        }
        params.push(l);
    }
    for p in &decl.params {
        params.push(ty_lifetimes(&p.ty, &generics, r));
    }
    let ret = decl
        .ret
        .as_ref()
        .map(|t| ty_lifetimes(t, &generics, r))
        .unwrap_or_default();
    (params, ret)
}

/// Which parameters (by index, `self` first) a call's result may borrow from.
///
/// Rules (SPEC §4 "Lifetimes"):
/// - a named lifetime in the return type flows from every parameter mentioning it;
/// - an elided lifetime flows from `&self`/`&mut self` if present, else from the
///   single reference parameter, else (ambiguous; reported at the definition)
///   from every reference parameter;
/// - a generic return type may carry borrows from any parameter.
pub fn return_flows(params: &[TyLifetimes], ret: &TyLifetimes, has_ref_self: bool) -> Vec<usize> {
    let mut out = BTreeSet::new();
    if ret.generic {
        out.extend(0..params.len());
    }
    for (i, p) in params.iter().enumerate() {
        if ret
            .named
            .iter()
            .any(|l| l != "static" && p.named.contains(l))
        {
            out.insert(i);
        }
    }
    if ret.elided {
        if has_ref_self {
            out.insert(0);
        } else {
            let refs: Vec<usize> = (0..params.len())
                .filter(|i| params[*i].has_lifetimes())
                .collect();
            out.extend(refs);
        }
    }
    out.into_iter().collect()
}

/// [`return_flows`] for a declared function.
pub fn decl_flows(decl: &FnDecl, outer: &[String], r: &Resolved<'_>) -> Vec<usize> {
    let (params, ret) = fn_lifetimes(decl, outer, r);
    let ref_self = matches!(decl.self_param, Some(SelfParam::Ref { .. }));
    return_flows(&params, &ret, ref_self)
}
