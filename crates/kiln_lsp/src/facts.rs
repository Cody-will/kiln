//! The navigation index ("facts") of a program: definitions, name
//! resolutions, local and expression types, members, and method calls.
//!
//! `kilnc --index` prints these lines (the self-hosted front end); [`rust`]
//! computes the same lines with the Rust front end (the fallback when kilnc
//! is not installed, and the reference for the differential test).
//! [`crate::analysis::from_facts`] turns them into an
//! [`crate::analysis::Analysis`].
//!
//! One fact per line, tab-separated, with global compiler spans (each file
//! at `k << 32`); text fields escape `\\`, newline and tab:
//!
//! ```text
//! def   KIND S E ITEM_S ITEM_E ROOT NAME       a definition in user code (ROOT: in the root module)
//! pre   KIND NAME SIG                          a prelude name (completion)
//! ref   S E D DEF_S DEF_E                      a path naming a user definition
//! ref   S E S KEY HOVER                        ... a std definition (KEY = name#start)
//! ref   S E L BIND_S BIND_E                    ... a local
//! bind  S E BIND_S BIND_E                      a binding site
//! meth  TYPE NAME_S NAME_E ITEM_S ITEM_E SELF NAME SIG   a method (SIG for std methods)
//! scope ITEM_S ITEM_E                          a user function body with results
//! local FN_S BIND_S BIND_E NAME TYPE           a local's type
//! ety   S E TYPE ADT                           an expression's type (ADT: its type name or empty)
//! field ADT NAME TYPE                          a field of a struct type seen in user code
//! mref  CALL_S CALL_E NAME D DEF_S DEF_E       a method call to a user method
//! mref  CALL_S CALL_E NAME S KEY HOVER         ... to a std method
//! ```
//!
//! The lines are sorted and deduplicated.

use kiln_lexer::{Span, BUILTIN_BASE};
use kiln_resolve::{DefKind, Res};

/// Escapes a text field.
pub fn field(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// Undoes [`field`].
pub fn unfield(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some(x) => o.push(x),
            None => o.push('\\'),
        }
    }
    o
}

/// The kind word of a definition.
pub fn kind_name(k: DefKind) -> &'static str {
    match k {
        DefKind::Mod => "mod",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Variant { .. } => "variant",
        DefKind::Trait => "trait",
        DefKind::Fn => "fun",
        DefKind::Const => "const",
        DefKind::Static => "static",
        DefKind::TypeAlias => "type",
        DefKind::External => "module",
    }
}

/// The first source line of an item, as a signature for hover.
pub fn signature(src: &str, start: usize) -> String {
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let line = src[line_start..].lines().next().unwrap_or("").trim();
    line.trim_end_matches('{').trim_end().to_string()
}

/// Doc comments (`//` lines directly above an item; attributes skipped).
pub fn doc_above(src: &str, start: usize) -> String {
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut docs = Vec::new();
    let mut end = line_start;
    while end > 0 {
        let prev_start = src[..end - 1].rfind('\n').map_or(0, |i| i + 1);
        let line = src[prev_start..end - 1].trim();
        if let Some(c) = line.strip_prefix("//") {
            docs.push(c.trim_start_matches('/').trim().to_string());
            end = prev_start;
        } else if line.starts_with('@') {
            end = prev_start;
        } else {
            break;
        }
    }
    docs.reverse();
    docs.join("\n")
}

/// Hover markdown: the signature, then the docs.
pub fn hover_text(sig: &str, doc: &str) -> String {
    if doc.is_empty() {
        format!("```kiln\n{sig}\n```")
    } else {
        format!("```kiln\n{sig}\n```\n\n{doc}")
    }
}

fn user(s: Span) -> bool {
    s.start < BUILTIN_BASE
}

/// The facts of a parsed program, from the Rust front end.
pub fn rust(file: &kiln_parser::ast::File) -> String {
    let prelude = kiln_resolve::parse_prelude();
    let r = kiln_resolve::resolve(file, &prelude);
    let builtin = &kiln_resolve::builtin().text;
    let std_at = |s: Span| -> Option<usize> {
        (s.start >= BUILTIN_BASE && s.start - BUILTIN_BASE < builtin.len())
            .then(|| s.start - BUILTIN_BASE)
    };
    let std_sig = |s: Span| std_at(s).map_or(String::new(), |o| signature(builtin, o));
    let std_ref = |name: &str, s: Span| -> String {
        let hover = std_at(s).map_or(String::new(), |o| {
            hover_text(&signature(builtin, o), &doc_above(builtin, o))
        });
        format!(
            "S\t{}\t{}",
            field(&format!("{name}#{}", s.start)),
            field(&hover)
        )
    };
    let mut out: Vec<String> = Vec::new();
    for d in &r.defs {
        if d.is_prelude || !user(d.span) || d.span.start >= d.span.end {
            continue;
        }
        if matches!(d.kind, DefKind::Mod) && d.name.starts_with('<') {
            continue;
        }
        let full = d.item.map_or(d.span, |i| i.span);
        out.push(format!(
            "def\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            kind_name(d.kind),
            d.span.start,
            d.span.end,
            full.start,
            full.end,
            u8::from(d.module == r.root),
            field(&d.name)
        ));
    }
    // Importable names (for "import" quick fixes): `pub` items outside
    // the root module and the prelude, with their module path.
    for d in &r.defs {
        if matches!(
            d.kind,
            DefKind::Mod | DefKind::Variant { .. } | DefKind::External
        ) || d.vis == kiln_parser::ast::Visibility::Private
            || d.module == r.root
            || d.module == r.prelude
            || d.name.starts_with(['_', '<'])
        {
            continue;
        }
        let mut path = Vec::new();
        let mut m = d.module;
        while let Some(p) = r.modules.get(&m).and_then(|s| s.parent) {
            path.push(r.def(m).name.as_str());
            m = p;
        }
        if path.iter().any(|n| n.starts_with('<')) {
            continue;
        }
        path.reverse();
        out.push(format!(
            "path\t{}\t{}\t{}",
            field(&d.name),
            kind_name(d.kind),
            field(&path.join("::"))
        ));
    }
    if let Some(scope) = r.modules.get(&r.prelude) {
        for (name, id) in scope.values.iter().chain(scope.types.iter()) {
            if name.starts_with("kn_") || name.starts_with('_') {
                continue;
            }
            let d = r.def(*id);
            out.push(format!(
                "pre\t{}\t{}\t{}",
                kind_name(d.kind),
                field(name),
                field(&std_sig(d.span))
            ));
        }
    }
    for (sp, res) in &r.paths {
        if !user(*sp) {
            continue;
        }
        let what = match res {
            Res::Def(id) => {
                let d = r.def(*id);
                if user(d.span) {
                    format!("D\t{}\t{}", d.span.start, d.span.end)
                } else {
                    std_ref(&d.name, d.span)
                }
            }
            Res::Local(l) => {
                let ls = r.locals[l.0 as usize].span;
                format!("L\t{}\t{}", ls.start, ls.end)
            }
            _ => continue,
        };
        out.push(format!("ref\t{}\t{}\t{what}", sp.start, sp.end));
    }
    for (sp, l) in &r.bindings {
        if user(*sp) {
            let ls = r.locals[l.0 as usize].span;
            out.push(format!(
                "bind\t{}\t{}\t{}\t{}",
                sp.start, sp.end, ls.start, ls.end
            ));
        }
    }
    // Types need a resolved program.
    if !r.errors.is_empty() {
        out.sort();
        out.dedup();
        return out.join("\n") + "\n";
    }
    let type_name_of = |ty: &kiln_parser::ast::Type| -> Option<String> {
        match r.paths.get(&ty.span)? {
            Res::Def(id) => Some(r.def(*id).name.clone()),
            Res::Prim(p) => Some(format!("{p:?}").to_lowercase()),
            _ => None,
        }
    };
    let t = kiln_typeck::typeck(&r);
    let strip = |ty: &kiln_typeck::ty::Ty| {
        let mut ty = ty.clone();
        while let kiln_typeck::ty::Ty::Ref(_, inner) = ty {
            ty = *inner;
        }
        ty
    };
    let adt_name = |ty: &kiln_typeck::ty::Ty| match strip(ty) {
        kiln_typeck::ty::Ty::Adt(id, _) => Some(r.def(id).name.clone()),
        kiln_typeck::ty::Ty::Prim(p) => Some(format!("{p:?}").to_lowercase()),
        _ => None,
    };
    for (i, f) in r.fns.iter().enumerate() {
        if let Some(ii) = f.impl_idx {
            if let Some(tn) = type_name_of(&r.impls[ii].decl.self_ty) {
                let ns = f.decl.name.span;
                out.push(format!(
                    "meth\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    field(&tn),
                    ns.start,
                    ns.end,
                    f.item.span.start,
                    f.item.span.end,
                    u8::from(f.decl.self_param.is_some()),
                    field(&f.decl.name.name),
                    field(&std_sig(ns))
                ));
            }
        }
        if !user(f.item.span) {
            continue;
        }
        let Some(res) = t.fn_results(i) else { continue };
        let fs = f.item.span;
        out.push(format!("scope\t{}\t{}", fs.start, fs.end));
        for (l, ty) in &res.locals {
            let loc = &r.locals[l.0 as usize];
            if user(loc.span) {
                out.push(format!(
                    "local\t{}\t{}\t{}\t{}\t{}",
                    fs.start,
                    loc.span.start,
                    loc.span.end,
                    field(&loc.name),
                    field(&t.fmt_ty(ty))
                ));
            }
        }
        for (sp, ty) in &res.tys {
            if !user(*sp) {
                continue;
            }
            let adt = adt_name(ty);
            out.push(format!(
                "ety\t{}\t{}\t{}\t{}",
                sp.start,
                sp.end,
                field(&t.fmt_ty(ty)),
                field(adt.as_deref().unwrap_or(""))
            ));
            if let Some(adt) = adt {
                for (fname, fty) in t.struct_fields(&strip(ty)) {
                    out.push(format!(
                        "field\t{}\t{}\t{}",
                        field(&adt),
                        field(&fname),
                        field(&t.fmt_ty(&fty))
                    ));
                }
            }
        }
        for (call, mi) in &res.methods {
            let Some(fi) = mi.fn_idx else { continue };
            if !user(*call) {
                continue;
            }
            let callee = &r.fns[fi];
            let def = callee.decl.name.span;
            let name = &callee.decl.name.name;
            let what = if user(def) {
                format!("D\t{}\t{}", def.start, def.end)
            } else {
                std_ref(name, def)
            };
            out.push(format!(
                "mref\t{}\t{}\t{}\t{what}",
                call.start,
                call.end,
                field(name)
            ));
        }
    }
    out.sort();
    out.dedup();
    out.join("\n") + "\n"
}
