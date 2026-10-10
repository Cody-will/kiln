//! Lifetime checks on signatures and struct definitions: undeclared lifetime
//! names and elided lifetimes the elision rules can't fill in.

use kiln_lexer::{Diagnostic, Span};
use kiln_mir::sig::{self, TyLifetimes};
use kiln_parser::ast::{
    Bound, FieldDecl, Fields, FnDecl, GenericArg, GenericParam, Generics, ItemKind, SelfParam,
    Type, TypeKind,
};
use kiln_resolve::Resolved;

fn lifetime_names(g: &Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Lifetime { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect()
}

/// Every named lifetime (with its span) mentioned in `t`.
fn used_lifetimes(t: &Type, out: &mut Vec<(String, Span)>) {
    match &t.kind {
        TypeKind::Ref {
            lifetime, inner, ..
        } => {
            if let Some(l) = lifetime {
                out.push((l.name.clone(), l.span));
            }
            used_lifetimes(inner, out);
        }
        TypeKind::Path(p) => {
            for seg in &p.segments {
                for a in &seg.args {
                    match a {
                        GenericArg::Lifetime(l) => out.push((l.name.clone(), l.span)),
                        GenericArg::Type(t) | GenericArg::Binding { ty: t, .. } => {
                            used_lifetimes(t, out)
                        }
                    }
                }
            }
        }
        TypeKind::Ptr { inner, .. }
        | TypeKind::Slice(inner)
        | TypeKind::Array(inner, _)
        | TypeKind::ChanSend(inner)
        | TypeKind::ChanRecv(inner) => used_lifetimes(inner, out),
        TypeKind::Tuple(ts) => ts.iter().for_each(|x| used_lifetimes(x, out)),
        TypeKind::Fn { params, ret } => {
            params.iter().for_each(|x| used_lifetimes(x, out));
            if let Some(r) = ret {
                used_lifetimes(r, out);
            }
        }
        TypeKind::Dyn(bs) | TypeKind::Impl(bs) => {
            for b in bs {
                if let Bound::Lifetime(l) = b {
                    out.push((l.name.clone(), l.span));
                }
            }
        }
        TypeKind::Never | TypeKind::Infer => {}
    }
}

fn check_declared(types: &[&Type], declared: &[String], errors: &mut Vec<Diagnostic>) {
    let mut used = vec![];
    for t in types {
        used_lifetimes(t, &mut used);
    }
    for (name, span) in used {
        if name != "static" && !declared.contains(&name) {
            errors.push(
                Diagnostic::error(format!("use of undeclared lifetime name `'{name}`"), span)
                    .with_help(format!("declare it in the generic parameters: `<'{name}>`")),
            );
        }
    }
}

/// Lifetimes named in outlives bounds (`<'a: 'b>`, `where 'a: 'b`) must be declared.
fn check_bound_names(gs: &[&Generics], declared: &[String], errors: &mut Vec<Diagnostic>) {
    let mut names: Vec<&kiln_parser::ast::Ident> = vec![];
    for g in gs {
        for p in &g.params {
            if let GenericParam::Lifetime { bounds, .. } = p {
                names.extend(bounds);
            }
        }
        for w in &g.where_clause {
            if let Some(l) = &w.lifetime {
                names.push(l);
                for b in &w.bounds {
                    if let Bound::Lifetime(x) = b {
                        names.push(x);
                    }
                }
            }
        }
    }
    for n in names {
        if n.name != "static" && !declared.contains(&n.name) {
            errors.push(
                Diagnostic::error(
                    format!("use of undeclared lifetime name `'{}`", n.name),
                    n.span,
                )
                .with_help(format!(
                    "declare it in the generic parameters: `<'{}>`",
                    n.name
                )),
            );
        }
    }
}

/// Checks one function signature.
pub fn check_fn(
    decl: &FnDecl,
    outer: &Generics,
    outer_types: &[String],
    r: &Resolved<'_>,
    errors: &mut Vec<Diagnostic>,
) {
    let mut declared = lifetime_names(outer);
    declared.extend(lifetime_names(&decl.generics));
    let mut types: Vec<&Type> = decl.params.iter().map(|p| &p.ty).collect();
    if let Some(t) = &decl.ret {
        types.push(t);
    }
    check_declared(&types, &declared, errors);
    check_bound_names(&[outer, &decl.generics], &declared, errors);
    if let Some(SelfParam::Ref {
        lifetime: Some(l), ..
    }) = &decl.self_param
    {
        if l.name != "static" && !declared.contains(&l.name) {
            errors.push(Diagnostic::error(
                format!("use of undeclared lifetime name `'{}`", l.name),
                l.span,
            ));
        }
    }
    let Some(ret_ty) = &decl.ret else { return };
    let (params, ret) = sig::fn_lifetimes(decl, outer_types, r);
    if !ret.elided || matches!(decl.self_param, Some(SelfParam::Ref { .. })) {
        return;
    }
    let offset = usize::from(decl.self_param.is_some());
    let with: Vec<String> = params
        .iter()
        .enumerate()
        .filter(|(_, p)| TyLifetimes::has_lifetimes(p))
        .map(|(i, _)| match i.checked_sub(offset) {
            Some(j) => param_name(decl, j),
            None => "self".into(),
        })
        .collect();
    if with.len() == 1 {
        return;
    }
    let help = if with.is_empty() {
        "the return type contains a borrowed value, but there is no parameter for it to be borrowed from; return an owned value (e.g. `String`) or use `'static`".to_string()
    } else {
        let names = with
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        format!(
            "the return type contains a borrowed value, but the signature does not say whether it is borrowed from {names}; add a lifetime parameter, e.g. `fn {}<'a>(..) -> &'a T`",
            decl.name.name
        )
    };
    errors.push(Diagnostic::error("missing lifetime specifier", ret_ty.span).with_help(help));
}

fn param_name(decl: &FnDecl, j: usize) -> String {
    match &decl.params[j].pat.kind {
        kiln_parser::ast::PatternKind::Ident { name, .. } => name.name.clone(),
        _ => format!("parameter {}", j + 1),
    }
}

fn check_fields(
    fields: &[&FieldDecl],
    tys: &[&Type],
    generics: &Generics,
    r: &Resolved<'_>,
    errors: &mut Vec<Diagnostic>,
) {
    let declared = lifetime_names(generics);
    check_declared(tys, &declared, errors);
    let types = sig::type_param_names(generics);
    for t in tys {
        if sig::ty_lifetimes(t, &types, r).elided {
            let _ = fields;
            errors.push(
                Diagnostic::error("missing lifetime specifier", t.span)
                    .with_help("a struct that holds a reference needs a lifetime parameter: `struct S<'a> { field: &'a T }`"),
            );
        }
    }
}

/// Checks struct and enum definitions.
pub fn check_items(r: &Resolved<'_>, errors: &mut Vec<Diagnostic>) {
    for def in &r.defs {
        if def.is_prelude {
            continue;
        }
        let Some(item) = def.item else { continue };
        match &item.kind {
            ItemKind::Struct(s) => {
                let (fs, ts) = field_types(&s.fields);
                check_fields(&fs, &ts, &s.generics, r, errors);
            }
            ItemKind::Enum(e) => {
                let mut fs = vec![];
                let mut ts = vec![];
                for v in &e.variants {
                    let (f, t) = field_types(&v.fields);
                    fs.extend(f);
                    ts.extend(t);
                }
                check_fields(&fs, &ts, &e.generics, r, errors);
            }
            _ => {}
        }
    }
}

fn field_types(f: &Fields) -> (Vec<&FieldDecl>, Vec<&Type>) {
    match f {
        Fields::Unit => (vec![], vec![]),
        Fields::Tuple(ts) => (vec![], ts.iter().collect()),
        Fields::Named(fs) => (fs.iter().collect(), fs.iter().map(|f| &f.ty).collect()),
    }
}
