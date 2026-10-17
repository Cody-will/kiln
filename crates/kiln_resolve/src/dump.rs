//! A canonical, line-oriented dump of a [`Resolved`] table, used by the
//! differential test of the self-hosted resolver (compiler/src/resolve.kn).
//!
//! The user file is parsed first, then the prelude (so derive spans are
//! allocated in that order, as in `kiln build`); this is only stable in a
//! fresh process. Entries whose span lies in the built-in sources
//! (`BUILTIN_BASE..DERIVE_BASE`) are left out; ids still count them.

use crate::*;
use std::fmt::Write;

fn builtin(s: Span) -> bool {
    s.start >= kiln_lexer::BUILTIN_BASE && s.start < kiln_parser::DERIVE_BASE
}

fn res(r: &Res) -> String {
    match r {
        Res::Def(d) => format!("d{}", d.0),
        Res::Local(l) => format!("l{}", l.0),
        Res::Prim(p) => format!("prim {p:?}"),
        Res::TyParam(n) => format!("typaram {n}"),
        Res::SelfTy => "Self".into(),
        Res::Builtin(b) => format!("builtin {b:?}"),
        Res::Assoc { base, name } => format!("assoc({}) {name}", res(base)),
        Res::External => "external".into(),
        Res::Err => "err".into(),
    }
}

fn opt<T>(o: Option<T>, f: impl Fn(T) -> String) -> String {
    o.map(f).unwrap_or_else(|| "-".into())
}

/// Resolves `src` and prints the table.
pub fn dump(src: &str) -> String {
    let parsed = kiln_parser::parse(src);
    let prelude = parse_prelude();
    let r = resolve(&parsed.file, &prelude);
    let mut o = String::new();
    let _ = writeln!(
        o,
        "defs {} root d{} prelude d{}",
        r.defs.len(),
        r.root.0,
        r.prelude.0
    );
    for (i, d) in r.defs.iter().enumerate() {
        if d.is_prelude || builtin(d.span) {
            continue;
        }
        let kind = match d.kind {
            DefKind::Variant { parent, index } => format!("variant(d{},{index})", parent.0),
            k => kind_name(k).replace(' ', "_"),
        };
        let _ = writeln!(
            o,
            "d{i} {kind} {} {}..{} {:?} m{}",
            d.name, d.span.start, d.span.end, d.vis, d.module.0
        );
    }
    let _ = writeln!(o, "locals {}", r.locals.len());
    for (i, l) in r.locals.iter().enumerate() {
        if !builtin(l.span) {
            let m = if l.mutable { " mut" } else { "" };
            let _ = writeln!(o, "l{i} {}{m} {}..{}", l.name, l.span.start, l.span.end);
        }
    }
    let mut paths: Vec<_> = r.paths.iter().filter(|(s, _)| !builtin(**s)).collect();
    paths.sort_by_key(|(s, _)| (s.start, s.end));
    o.push_str("paths\n");
    for (s, x) in paths {
        let _ = writeln!(o, "{}..{} {}", s.start, s.end, res(x));
    }
    let mut binds: Vec<_> = r.bindings.iter().filter(|(s, _)| !builtin(**s)).collect();
    binds.sort_by_key(|(s, _)| (s.start, s.end));
    o.push_str("bindings\n");
    for (s, l) in binds {
        let _ = writeln!(o, "{}..{} l{}", s.start, s.end, l.0);
    }
    let _ = writeln!(o, "impls {}", r.impls.len());
    for (i, im) in r.impls.iter().enumerate() {
        if !im.is_prelude {
            let _ = writeln!(
                o,
                "i{i} {}..{} m{}",
                im.span.start, im.span.end, im.module.0
            );
        }
    }
    let _ = writeln!(o, "fns {}", r.fns.len());
    for (i, f) in r.fns.iter().enumerate() {
        if f.is_prelude {
            continue;
        }
        let _ = writeln!(
            o,
            "f{i} {} {}..{} m{} impl={} trait={} def={} self={}",
            f.decl.name.name,
            f.decl.name.span.start,
            f.decl.name.span.end,
            f.module.0,
            opt(f.impl_idx, |x| x.to_string()),
            opt(f.trait_def, |x| format!("d{}", x.0)),
            opt(f.def, |x| format!("d{}", x.0)),
            opt(f.self_local, |x| format!("l{}", x.0)),
        );
    }
    o.push_str("statics\n");
    for (d, f) in &r.statics {
        let _ = writeln!(o, "d{} f{f}", d.0);
    }
    let mut cf: Vec<_> = r.const_fns.iter().map(|(d, f)| (d.0, *f)).collect();
    cf.sort();
    o.push_str("const_fns\n");
    for (d, f) in cf {
        let _ = writeln!(o, "d{d} f{f}");
    }
    let mut cs: Vec<_> = r.const_fn_set.iter().copied().collect();
    cs.sort();
    let _ = writeln!(o, "const_fn_set {cs:?}");
    for e in &r.errors {
        let _ = writeln!(o, "error {}..{}: {}", e.span.start, e.span.end, e.message);
        if let Some(h) = &e.help {
            let _ = writeln!(o, "  help: {h}");
        }
    }
    o
}
