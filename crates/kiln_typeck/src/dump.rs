//! A canonical dump of type checking, used by the differential test of the
//! self-hosted type checker (compiler/src/typeck.kn).
//!
//! The file is parsed, resolved (with the prelude) and type-checked even
//! when resolution reported errors. The dump lists every type error, then,
//! for each function body outside the prelude and standard library, its
//! results sorted by id or span: local types, expression types, method
//! resolutions, callees with their substitutions, coercions, deref
//! coercions, callee derefs, constant uses, and `for` element types.
//! Types print with definition ids (`d12<i32>`), so names cannot collide.

use crate::ty::{Ty, VarKind};
use crate::Typed;
use kiln_parser::ast::SelfParam;
use std::collections::HashMap;
use std::fmt::Write;

fn prim(p: kiln_resolve::PrimTy) -> &'static str {
    crate::display::prim_name(p)
}

/// Canonical text of a (zonked) type.
pub fn ty(tc: &Typed<'_, '_>, t: &Ty) -> String {
    let t = tc.c.infer.zonk(t);
    let all = |ts: &[Ty]| ts.iter().map(|x| ty(tc, x)).collect::<Vec<_>>().join(", ");
    match &t {
        Ty::Prim(p) => prim(*p).into(),
        Ty::Unit => "()".into(),
        Ty::Never => "!".into(),
        Ty::Tuple(ts) => format!("({})", all(ts)),
        Ty::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, ty(tc, x)),
        Ty::Ptr(m, x) => format!("*{} {}", if *m { "mut" } else { "const" }, ty(tc, x)),
        Ty::Array(x, n) => format!("[{}; {n}]", ty(tc, x)),
        Ty::Slice(x) => format!("[{}]", ty(tc, x)),
        Ty::Adt(d, args) => format!("d{}<{}>", d.0, all(args)),
        Ty::Fn(ps, r, m) => format!(
            "fun{}({}) {}",
            if *m { " mut" } else { "" },
            all(ps),
            ty(tc, r)
        ),
        Ty::Param(n) => format!("'{n}"),
        Ty::Dyn(ds) => format!(
            "dyn {}",
            ds.iter()
                .map(|d| format!("d{}", d.0))
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        Ty::ChanSend(x) => format!("chan<- {}", ty(tc, x)),
        Ty::ChanRecv(x) => format!("<-chan {}", ty(tc, x)),
        Ty::Var(v) => match tc.c.infer.kind(*v) {
            VarKind::Int => "?int".into(),
            VarKind::Float => "?float".into(),
            VarKind::General => "?".into(),
        },
        Ty::Unknown => "{unknown}".into(),
    }
}

fn substs(tc: &Typed<'_, '_>, m: &HashMap<String, Ty>) -> String {
    let mut v: Vec<_> = m
        .iter()
        .map(|(k, t)| format!("{k}={}", ty(tc, t)))
        .collect();
    v.sort();
    v.join(" ")
}

fn sp(s: kiln_lexer::Span) -> String {
    format!("{}..{}", s.start, s.end)
}

fn sorted<V>(m: &HashMap<kiln_lexer::Span, V>) -> Vec<(kiln_lexer::Span, &V)> {
    let mut v: Vec<_> = m.iter().map(|(k, x)| (*k, x)).collect();
    v.sort_by_key(|(s, _)| (s.start, s.end));
    v
}

/// Type-checks `src` and prints the results.
pub fn dump(src: &str) -> String {
    let parsed = kiln_parser::parse(src);
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    let tc = crate::typeck(&resolved);
    let mut o = String::new();
    for e in tc.errors() {
        let _ = writeln!(o, "error {}: {}", sp(e.span), e.message);
        if let Some(h) = &e.help {
            let _ = writeln!(o, "  help: {h}");
        }
    }
    for (i, f) in resolved.fns.iter().enumerate() {
        if f.is_prelude {
            continue;
        }
        let Some(r) = tc.fn_results(i) else { continue };
        let _ = writeln!(o, "fn f{i} {}", f.decl.name.name);
        let mut locals: Vec<_> = r.locals.iter().collect();
        locals.sort_by_key(|(l, _)| l.0);
        for (l, t) in locals {
            let _ = writeln!(o, "  local l{} {}", l.0, ty(&tc, t));
        }
        for (s, t) in sorted(&r.tys) {
            let _ = writeln!(o, "  expr {} {}", sp(s), ty(&tc, t));
        }
        for (s, m) in sorted(&r.methods) {
            let sp_ = match &m.self_param {
                None => "-",
                Some(SelfParam::Value { .. }) => "self",
                Some(SelfParam::Ref { mutable: false, .. }) => "&self",
                Some(SelfParam::Ref { mutable: true, .. }) => "&mut self",
            };
            let fi = m.fn_idx.map_or("-".into(), |i| format!("f{i}"));
            let _ = writeln!(
                o,
                "  method {} steps={} {sp_} recv={} fn={fi} [{}]",
                sp(s),
                m.steps,
                ty(&tc, &m.recv_ty),
                substs(&tc, &m.substs)
            );
        }
        for (s, c) in sorted(&r.callees) {
            let su = r
                .callee_substs
                .get(&s)
                .map_or(String::new(), |m| substs(&tc, m));
            let _ = writeln!(o, "  callee {} f{c} [{su}]", sp(s));
        }
        for (s, t) in sorted(&r.coercions) {
            let _ = writeln!(o, "  coerce {} {}", sp(s), ty(&tc, t));
        }
        for (s, n) in sorted(&r.deref_coercions) {
            let _ = writeln!(o, "  deref_coerce {} {n}", sp(s));
        }
        for (s, n) in sorted(&r.fn_derefs) {
            let _ = writeln!(o, "  fn_deref {} {n}", sp(s));
        }
        let mut cu: Vec<_> = r.const_uses.iter().collect();
        cu.sort_by_key(|s| (s.start, s.end));
        for s in cu {
            let _ = writeln!(o, "  const_use {}", sp(*s));
        }
        for (s, t) in sorted(&r.iter_items) {
            let _ = writeln!(o, "  iter_item {} {}", sp(s), ty(&tc, t));
        }
    }
    o
}
