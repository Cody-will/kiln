//! Semantic facts about one program, computed by the compiler front end
//! (parse, resolve, type check, borrow check) and then queried by the server.
//!
//! The language server debounces edits and runs this on a background thread;
//! each successful run replaces the previous analysis for that project.

use kiln_lexer::{Diagnostic, Span};
use kiln_resolve::{DefKind, Res};
use std::collections::HashMap;

/// What a name refers to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A definition in this file (its name span).
    Def(Span),
    /// A local variable (its binding span).
    Local(Span),
    /// A definition in the standard library or prelude (name, kind).
    Std(String),
}

/// A symbol for the outline.
#[derive(Clone, Debug)]
pub struct Symbol {
    /// Its name.
    pub name: String,
    /// What it is: "fun", "struct", "enum", "trait", "const", "static", "type", "mod", "method", "variant".
    pub kind: &'static str,
    /// The name's span.
    pub span: Span,
    /// The whole item's span.
    pub full: Span,
    /// The enclosing type for methods (`impl Point`).
    pub container: Option<String>,
}

/// A completion candidate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Completion {
    /// The text to insert.
    pub label: String,
    /// "fun", "method", "field", "local", "struct", "enum", "trait", "const", "keyword", "module", ...
    pub kind: &'static str,
    /// A type or signature.
    pub detail: String,
}

/// Facts about one file.
#[derive(Default, Debug)]
pub struct Analysis {
    /// Every diagnostic (parse, resolve, type, borrow).
    pub diagnostics: Vec<Diagnostic>,
    /// Name occurrences: (span of the name, what it refers to).
    pub refs: Vec<(Span, Target)>,
    /// Names of definitions in this file, by name span.
    pub def_names: HashMap<Span, String>,
    /// Hover text for definitions in this file, by name span.
    pub def_hover: HashMap<Span, String>,
    /// Hover text for standard-library names.
    pub std_hover: HashMap<String, String>,
    /// Local variables: binding span → (name, type).
    pub locals: HashMap<Span, (String, String)>,
    /// Binding spans that have no type annotation (for inlay hints).
    pub inferred: Vec<Span>,
    /// Expression types: (span, type), for hover on any expression.
    pub expr_tys: Vec<(Span, String)>,
    /// The outline.
    pub symbols: Vec<Symbol>,
    /// Top-level names for completion.
    pub globals: Vec<Completion>,
    /// Members (fields and methods) by type name.
    pub members: HashMap<String, Vec<Completion>>,
    /// Expression spans with the name of their type (for `.` completion).
    pub expr_type_names: Vec<(Span, String)>,
    /// The package's files laid end to end: (offset, path, length). All
    /// spans in this struct are offsets into that concatenation.
    pub files: Vec<(usize, std::path::PathBuf, usize)>,
    /// The concatenated text.
    pub text: String,
    /// Function bodies: (body span, local binding spans in it).
    pub scopes: Vec<(Span, Vec<Span>)>,
}

fn ident_at_end(src: &str, sp: Span) -> Span {
    let text = &src[sp.start..sp.end];
    let trimmed = text.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
    let end = sp.start + trimmed.len();
    let start = sp.start
        + trimmed
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
            .last()
            .map_or(trimmed.len(), |(i, _)| i);
    Span::new(start, end)
}

/// The first source line of an item, as a signature for hover.
fn signature(src: &str, start: usize) -> String {
    // Back up to the start of the line (to include `pub fn`).
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let rest = &src[line_start..];
    let line = rest.lines().next().unwrap_or("").trim();
    line.trim_end_matches('{').trim_end().to_string()
}

/// Doc comments (`//` lines directly above an item).
fn doc_above(src: &str, start: usize) -> String {
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

fn kind_name(k: DefKind) -> &'static str {
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

/// Spans of `.name` at bracket depth 0 within a call's span (the method name).
fn method_name_span(src: &str, call: Span, name: &str) -> Option<Span> {
    let text = &src[call.start..call.end];
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut found = None;
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'.' if depth == 0 && text[i + 1..].starts_with(name) => {
                    let after = text[i + 1 + name.len()..].chars().next();
                    if !after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        found = Some(Span::new(
                            call.start + i + 1,
                            call.start + i + 1 + name.len(),
                        ));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    found
}

/// Runs the front end over a single file.
pub fn analyze(src: &str) -> Analysis {
    let pkg = kiln_parser::package::Package {
        parse: kiln_parser::parse(src),
        files: vec![(0, std::path::PathBuf::from("main.kn"), src.to_string())],
    };
    analyze_package(&pkg)
}

/// Runs the front end over a loaded package (all its files).
///
/// The compiler gives each file its own span base (`k << 32`). Here the
/// files are laid end to end in one string, `text`, and every span is
/// converted to an offset in it, so the queries below work across files.
/// [`Analysis::files`] maps offsets back to files.
pub fn analyze_package(pkg: &kiln_parser::package::Package) -> Analysis {
    let parsed = &pkg.parse;
    let mut layout = Vec::new();
    let mut text = String::new();
    for (base, path, t) in &pkg.files {
        layout.push((*base, text.len(), path.clone(), t.len()));
        text.push_str(t);
        text.push('\n');
    }
    let map = |s: Span| -> Option<Span> {
        let (base, v, _, len) = layout
            .iter()
            .filter(|(b, _, _, _)| *b <= s.start)
            .max_by_key(|(b, _, _, _)| *b)?;
        if s.end > base + len || s.start >= kiln_lexer::BUILTIN_BASE {
            return None;
        }
        Some(Span::new(s.start - base + v, s.end - base + v))
    };
    let mut a = Analysis {
        diagnostics: kiln_borrowck::check_parse(parsed)
            .into_iter()
            .map(|mut d| {
                d.span = map(d.span).unwrap_or(Span::new(0, 0));
                d.notes = d
                    .notes
                    .into_iter()
                    .map(|(m, sp)| (m, sp.and_then(map)))
                    .collect();
                d
            })
            .collect(),
        files: layout
            .iter()
            .map(|(_, v, p, len)| (*v, p.clone(), *len))
            .collect(),
        ..Default::default()
    };
    let src = text.as_str();
    let prelude = kiln_resolve::parse_prelude();
    let r = kiln_resolve::resolve(&parsed.file, &prelude);
    let t = kiln_typeck::typeck(&r);
    let in_file = |s: Span| map(s).is_some();
    let m = |s: Span| map(s).unwrap_or(s);
    let builtin = kiln_resolve::builtin();
    let std_text = |s: Span| -> Option<(&str, usize)> {
        let base = kiln_lexer::BUILTIN_BASE;
        if s.start >= base && s.start - base < builtin.text.len() {
            Some((builtin.text.as_str(), s.start - base))
        } else {
            None
        }
    };

    // Definitions.
    for d in &r.defs {
        if matches!(d.kind, DefKind::Mod) && d.name.starts_with('<') {
            continue;
        }
        if in_file(d.span) && d.span.start < d.span.end {
            let ds = m(d.span);
            a.def_names.insert(ds, d.name.clone());
            let doc = doc_above(src, ds.start);
            let sig = signature(src, ds.start);
            a.def_hover.insert(ds, hover_text(&sig, &doc));
            if d.module == r.root || matches!(d.kind, DefKind::Variant { .. }) {
                let full = m(d.item.map_or(d.span, |i| i.span));
                a.symbols.push(Symbol {
                    name: d.name.clone(),
                    kind: kind_name(d.kind),
                    span: ds,
                    full,
                    container: None,
                });
            }
            if d.module == r.root && !matches!(d.kind, DefKind::Variant { .. }) {
                a.globals.push(Completion {
                    label: d.name.clone(),
                    kind: kind_name(d.kind),
                    detail: sig,
                });
            }
        }
    }
    // Prelude names for completion.
    if let Some(scope) = r.modules.get(&r.prelude) {
        for (name, id) in scope.values.iter().chain(scope.types.iter()) {
            let d = r.def(*id);
            if name.starts_with("kn_") || name.starts_with('_') {
                continue;
            }
            let detail = std_text(d.span)
                .map(|(t, o)| signature(t, o))
                .unwrap_or_default();
            a.globals.push(Completion {
                label: name.clone(),
                kind: kind_name(d.kind),
                detail,
            });
        }
    }

    // Uses of names.
    for (sp, res) in &r.paths {
        if !in_file(*sp) {
            continue;
        }
        let name_span = ident_at_end(src, m(*sp));
        match res {
            Res::Def(id) => {
                let d = r.def(*id);
                if in_file(d.span) {
                    a.refs.push((name_span, Target::Def(m(d.span))));
                } else {
                    let key = format!("{}#{}", d.name, d.span.start);
                    if let Some((text, off)) = std_text(d.span) {
                        a.std_hover.entry(key.clone()).or_insert_with(|| {
                            hover_text(&signature(text, off), &doc_above(text, off))
                        });
                    }
                    a.refs.push((name_span, Target::Std(key)));
                }
            }
            Res::Local(l) => {
                let ls = m(r.locals[l.0 as usize].span);
                a.refs.push((name_span, Target::Local(ls)));
            }
            _ => {}
        }
    }
    // Binding sites refer to themselves.
    for (sp, l) in &r.bindings {
        if in_file(*sp) {
            let ls = m(r.locals[l.0 as usize].span);
            let sp = &m(*sp);
            a.refs.push((*sp, Target::Local(ls)));
            // `x: T` (annotated binding or parameter) needs no hint.
            let rest = src[sp.end..].trim_start();
            let annotated = rest.starts_with(':') && !rest.starts_with(":=");
            if !annotated {
                a.inferred.push(*sp);
            }
        }
    }

    // Per-function types, methods, and members.
    for (i, f) in r.fns.iter().enumerate() {
        let in_this = in_file(f.item.span);
        // Members of in-file and std types alike, for `.` completion.
        if let Some(ii) = f.impl_idx {
            let imp = &r.impls[ii];
            let self_name = type_name_of(&r, &imp.decl.self_ty);
            if let Some(tn) = self_name {
                if f.decl.self_param.is_some() && !f.decl.name.name.starts_with('_') {
                    let detail = if in_this {
                        signature(src, m(f.decl.name.span).start)
                    } else {
                        std_text(f.decl.name.span)
                            .map(|(t, o)| signature(t, o))
                            .unwrap_or_default()
                    };
                    a.members.entry(tn.clone()).or_default().push(Completion {
                        label: f.decl.name.name.clone(),
                        kind: "method",
                        detail,
                    });
                }
                if in_this {
                    a.def_names
                        .insert(m(f.decl.name.span), f.decl.name.name.clone());
                    let sig = signature(src, m(f.decl.name.span).start);
                    a.def_hover.insert(
                        m(f.decl.name.span),
                        hover_text(&sig, &doc_above(src, m(f.decl.name.span).start)),
                    );
                    a.symbols.push(Symbol {
                        name: f.decl.name.name.clone(),
                        kind: "method",
                        span: m(f.decl.name.span),
                        full: m(f.item.span),
                        container: Some(tn),
                    });
                }
            }
        }
        if !in_this {
            continue;
        }
        let Some(res) = t.fn_results(i) else { continue };
        let mut scope = Vec::new();
        for (l, ty) in &res.locals {
            let loc = &r.locals[l.0 as usize];
            if in_file(loc.span) {
                a.locals
                    .insert(m(loc.span), (loc.name.clone(), t.fmt_ty(ty)));
                scope.push(m(loc.span));
            }
        }
        a.scopes.push((m(f.item.span), scope));
        for (sp, ty) in &res.tys {
            if in_file(*sp) {
                let sp = &m(*sp);
                a.expr_tys.push((*sp, t.fmt_ty(ty)));
                if let Some(name) = adt_name(&r, ty) {
                    a.expr_type_names.push((*sp, name.clone()));
                    // Fields.
                    let fields = t.struct_fields(&strip_refs(ty));
                    if !fields.is_empty() {
                        let e = a.members.entry(name).or_default();
                        for (fname, fty) in fields {
                            let c = Completion {
                                label: fname,
                                kind: "field",
                                detail: t.fmt_ty(&fty),
                            };
                            if !e.contains(&c) {
                                e.push(c);
                            }
                        }
                    }
                }
            }
        }
        for (call, mi) in &res.methods {
            let Some(fi) = mi.fn_idx else { continue };
            let callee = &r.fns[fi];
            let Some(ns) = method_name_span(src, m(*call), &callee.decl.name.name) else {
                continue;
            };
            let def = callee.decl.name.span;
            if in_file(def) {
                a.refs.push((ns, Target::Def(m(def))));
            } else {
                let key = format!("{}#{}", callee.decl.name.name, def.start);
                if let Some((text, off)) = std_text(def) {
                    a.std_hover.entry(key.clone()).or_insert_with(|| {
                        hover_text(&signature(text, off), &doc_above(text, off))
                    });
                }
                a.refs.push((ns, Target::Std(key)));
            }
        }
    }
    for v in a.members.values_mut() {
        v.sort();
        v.dedup_by(|x, y| x.label == y.label);
    }
    a.globals.sort();
    a.globals.dedup_by(|x, y| x.label == y.label);
    a.refs.sort_by_key(|(s, _)| (s.start, s.end));
    a.refs.dedup();
    a.symbols.sort_by_key(|s| s.span.start);
    a.text = text.clone();
    a.expr_tys
        .sort_by_key(|(s, _)| (s.start, std::cmp::Reverse(s.end)));
    a
}

fn hover_text(sig: &str, doc: &str) -> String {
    if doc.is_empty() {
        format!("```kiln\n{sig}\n```")
    } else {
        format!("```kiln\n{sig}\n```\n\n{doc}")
    }
}

fn strip_refs(t: &kiln_typeck::ty::Ty) -> kiln_typeck::ty::Ty {
    match t {
        kiln_typeck::ty::Ty::Ref(_, inner) => strip_refs(inner),
        _ => t.clone(),
    }
}

fn adt_name(r: &kiln_resolve::Resolved<'_>, t: &kiln_typeck::ty::Ty) -> Option<String> {
    match strip_refs(t) {
        kiln_typeck::ty::Ty::Adt(id, _) => Some(r.def(id).name.clone()),
        kiln_typeck::ty::Ty::Prim(p) => Some(format!("{p:?}").to_lowercase()),
        _ => None,
    }
}

fn type_name_of(r: &kiln_resolve::Resolved<'_>, ty: &kiln_parser::ast::Type) -> Option<String> {
    // The type's path resolution, when it names an ADT.
    for (sp, res) in &r.paths {
        if *sp == ty.span {
            if let Res::Def(id) = res {
                return Some(r.def(*id).name.clone());
            }
            if let Res::Prim(p) = res {
                return Some(format!("{p:?}").to_lowercase());
            }
        }
    }
    None
}

impl Analysis {
    /// The definition a position refers to: a span in this file.
    pub fn definition(&self, at: usize) -> Option<Span> {
        if let Some(s) = self.def_names.keys().find(|s| s.start <= at && at <= s.end) {
            return Some(*s);
        }
        match self
            .refs
            .iter()
            .find(|(s, _)| s.start <= at && at <= s.end)?
            .1
        {
            Target::Def(s) | Target::Local(s) => Some(s),
            Target::Std(_) => None,
        }
    }

    /// All occurrences (including the definition) of what's at `at`.
    pub fn references(&self, at: usize) -> Vec<Span> {
        let target = if let Some(s) = self.def_names.keys().find(|s| s.start <= at && at <= s.end) {
            Target::Def(*s)
        } else {
            match self.refs.iter().find(|(s, _)| s.start <= at && at <= s.end) {
                Some((_, t)) => t.clone(),
                None => return vec![],
            }
        };
        let mut out: Vec<Span> = self
            .refs
            .iter()
            .filter(|(_, t)| *t == target)
            .map(|(s, _)| *s)
            .collect();
        if let Target::Def(d) | Target::Local(d) = target {
            if !out.contains(&d) {
                out.push(d);
            }
        }
        out.sort_by_key(|s| s.start);
        out.dedup();
        out
    }

    /// Hover text (Markdown) for `at`.
    pub fn hover(&self, at: usize) -> Option<(Span, String)> {
        if let Some((s, h)) = self
            .def_hover
            .iter()
            .find(|(s, _)| s.start <= at && at <= s.end)
        {
            return Some((*s, h.clone()));
        }
        if let Some((s, t)) = self.refs.iter().find(|(s, _)| s.start <= at && at <= s.end) {
            match t {
                Target::Local(l) => {
                    if let Some((name, ty)) = self.locals.get(l) {
                        return Some((*s, format!("```kiln\n{name} {ty}\n```")));
                    }
                }
                Target::Def(d) => {
                    if let Some(h) = self.def_hover.get(d) {
                        return Some((*s, h.clone()));
                    }
                }
                Target::Std(k) => {
                    if let Some(h) = self.std_hover.get(k) {
                        return Some((*s, h.clone()));
                    }
                }
            }
        }
        // The smallest expression containing the position.
        self.expr_tys
            .iter()
            .filter(|(s, _)| s.start <= at && at < s.end)
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(s, ty)| (*s, format!("```kiln\n{ty}\n```")))
    }

    /// Inlay hints: (offset after the binding name, " Type").
    pub fn inlay_hints(&self) -> Vec<(usize, String)> {
        let mut v: Vec<(usize, String)> = self
            .inferred
            .iter()
            .filter_map(|s| {
                let (name, ty) = self.locals.get(s)?;
                if name == "_" || name == "self" || ty.contains('?') || ty == "{unknown}" {
                    return None;
                }
                Some((s.end, format!(" {ty}")))
            })
            .collect();
        v.sort();
        v
    }

    /// Completions at `at` in `src`.
    pub fn completions(&self, src: &str, at: usize) -> Vec<Completion> {
        let before = &src[..at.min(src.len())];
        let word_start = before
            .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
            .len();
        let prefix = &before[word_start..];
        let mut out = Vec::new();
        if before[..word_start].ends_with('.') && !before[..word_start].ends_with("..") {
            // Member completion: the type of the expression before the dot.
            let dot = word_start - 1;
            if let Some((_, ty)) = self
                .expr_type_names
                .iter()
                .filter(|(s, _)| s.end == dot)
                .min_by_key(|(s, _)| s.end - s.start)
            {
                if let Some(ms) = self.members.get(ty) {
                    out.extend(ms.iter().cloned());
                }
            }
        } else if before[..word_start].ends_with("::") {
            // Paths: leave to the globals for now.
            out.extend(self.globals.iter().cloned());
        } else {
            // Locals of the enclosing function declared before the cursor.
            for (body, locals) in &self.scopes {
                if body.start <= at && at <= body.end {
                    for l in locals {
                        if l.end <= at {
                            if let Some((name, ty)) = self.locals.get(l) {
                                out.push(Completion {
                                    label: name.clone(),
                                    kind: "local",
                                    detail: ty.clone(),
                                });
                            }
                        }
                    }
                }
            }
            out.extend(self.globals.iter().cloned());
            for k in KEYWORDS {
                out.push(Completion {
                    label: k.to_string(),
                    kind: "keyword",
                    detail: String::new(),
                });
            }
        }
        out.retain(|c| c.label.starts_with(prefix));
        out.sort_by(|a, b| a.label.cmp(&b.label).then(a.kind.cmp(b.kind)));
        out.dedup_by(|x, y| x.label == y.label);
        out
    }
}

const KEYWORDS: &[&str] = &[
    "break", "chan", "const", "continue", "defer", "else", "enum", "false", "for", "fun", "spawn",
    "if", "impl", "import", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "return",
    "select", "self", "Self", "static", "struct", "trait", "true", "type", "unsafe", "where",
    "while",
];
