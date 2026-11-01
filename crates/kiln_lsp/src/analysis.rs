//! Semantic facts about one program, computed by the compiler front end
//! (parse, resolve, type check, borrow check) and then queried by the server.
//!
//! The language server debounces edits and runs this on a background thread;
//! each successful run replaces the previous analysis for that project.

use kiln_lexer::{Diagnostic, Span};
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
    /// Each file's global span base, offset in `text`, and length.
    pub bases: Vec<(usize, usize, usize)>,
    /// What each definition name span defines ("fun", "struct", "method", ...).
    pub def_kinds: HashMap<Span, &'static str>,
    /// Kinds of prelude names.
    pub pre_kinds: HashMap<String, &'static str>,
    /// Importable names: (name, kind, module path), for import quick fixes.
    pub importable: Vec<(String, &'static str, String)>,
}

/// A quick fix: a title and the edits (span in [`Analysis::text`], new text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// Shown in the editor's light-bulb menu.
    pub title: String,
    /// Replacements, all in one file.
    pub edits: Vec<(Span, String)>,
}

/// Semantic token types, in legend order (see [`Analysis::semantic_tokens`]).
pub const TOKEN_TYPES: [&str; 9] = [
    "namespace",
    "type",
    "struct",
    "enum",
    "interface",
    "enumMember",
    "function",
    "method",
    "variable",
];

fn token_type(kind: &str) -> Option<u32> {
    Some(match kind {
        "mod" | "module" => 0,
        "type" => 1,
        "struct" => 2,
        "enum" => 3,
        "trait" => 4,
        "variant" => 5,
        "fun" => 6,
        "method" => 7,
        "const" | "static" | "local" => 8,
        _ => return None,
    })
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
    analyze_package_with(pkg, true)
}

/// [`analyze_package`]; `diagnostics: false` leaves the diagnostics empty
/// (the language server takes them from kilnc, the self-hosted front end).
pub fn analyze_package_with(pkg: &kiln_parser::package::Package, diagnostics: bool) -> Analysis {
    let files: Vec<(usize, std::path::PathBuf, String)> = pkg.files.clone();
    let facts = crate::facts::rust(&pkg.parse.file);
    let mut a = from_facts(&files, &facts);
    if diagnostics {
        let diags = kiln_borrowck::check_parse(&pkg.parse);
        a.diagnostics = a.map_diagnostics(diags);
    }
    a
}

impl Analysis {
    /// Converts a global compiler span to an offset span in [`Analysis::text`].
    pub fn map_span(&self, s: Span) -> Option<Span> {
        let (base, v, len) = self
            .bases
            .iter()
            .filter(|(b, _, _)| *b <= s.start)
            .max_by_key(|(b, _, _)| *b)?;
        if s.start >= kiln_lexer::BUILTIN_BASE || s.end > base + len {
            return None;
        }
        Some(Span::new(s.start - base + v, s.end - base + v))
    }

    /// Maps diagnostics with global spans into this analysis.
    pub fn map_diagnostics(&self, ds: Vec<Diagnostic>) -> Vec<Diagnostic> {
        ds.into_iter()
            .map(|mut d| {
                d.span = self.map_span(d.span).unwrap_or(Span::new(0, 0));
                d.notes = d
                    .notes
                    .into_iter()
                    .map(|(m, sp)| (m, sp.and_then(|s| self.map_span(s))))
                    .collect();
                d
            })
            .collect()
    }
}

fn kind_static(k: &str) -> &'static str {
    const KINDS: [&str; 11] = [
        "mod", "struct", "enum", "variant", "trait", "fun", "const", "static", "type", "module",
        "method",
    ];
    KINDS.iter().find(|x| **x == k).copied().unwrap_or("module")
}

/// Builds the analysis of a program from its files (span base, path, text)
/// and its navigation facts (see [`crate::facts`]).
pub fn from_facts(files: &[(usize, std::path::PathBuf, String)], facts: &str) -> Analysis {
    use crate::facts::{doc_above, hover_text, signature, unfield};
    let mut a = Analysis::default();
    let mut text = String::new();
    for (base, path, t) in files {
        a.bases.push((*base, text.len(), t.len()));
        a.files.push((text.len(), path.clone(), t.len()));
        text.push_str(t);
        text.push('\n');
    }
    a.text = text;
    // Scopes first: the lines are sorted, and `local` lines refer to them.
    let mut scopes: Vec<(usize, Span, Vec<Span>)> = Vec::new();
    for line in facts.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if let ["scope", s, e] = f.as_slice() {
            let (Ok(s), Ok(e)) = (s.parse(), e.parse()) else {
                continue;
            };
            if let Some(full) = a.map_span(Span::new(s, e)) {
                scopes.push((s, full, Vec::new()));
            }
        }
    }
    for line in facts.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let n = |i: usize| -> usize { f.get(i).and_then(|x| x.parse().ok()).unwrap_or(0) };
        let sp = |i: usize| Span::new(n(i), n(i + 1));
        let txt = |i: usize| unfield(f.get(i).copied().unwrap_or(""));
        let src = a.text.as_str();
        match f[0] {
            "def" => {
                let Some(ds) = a.map_span(sp(2)) else {
                    continue;
                };
                let name = txt(7);
                let kind = kind_static(f[1]);
                let sig = signature(src, ds.start);
                a.def_names.insert(ds, name.clone());
                a.def_kinds.insert(ds, kind);
                a.def_hover
                    .insert(ds, hover_text(&sig, &doc_above(src, ds.start)));
                let root = f[6] == "1";
                if root || kind == "variant" {
                    let full = a.map_span(sp(4)).unwrap_or(ds);
                    a.symbols.push(Symbol {
                        name: name.clone(),
                        kind,
                        span: ds,
                        full,
                        container: None,
                    });
                }
                if root && kind != "variant" {
                    a.globals.push(Completion {
                        label: name,
                        kind,
                        detail: sig,
                    });
                }
            }
            "pre" => {
                a.pre_kinds.insert(txt(2), kind_static(f[1]));
                a.globals.push(Completion {
                    label: txt(2),
                    kind: kind_static(f[1]),
                    detail: txt(3),
                })
            }
            "path" => a.importable.push((txt(1), kind_static(f[2]), txt(3))),
            "ref" => {
                let Some(at) = a.map_span(sp(1)) else {
                    continue;
                };
                let name_span = ident_at_end(src, at);
                let target = match f[3] {
                    "D" => match a.map_span(sp(4)) {
                        Some(d) => Target::Def(d),
                        None => continue,
                    },
                    "L" => match a.map_span(sp(4)) {
                        Some(l) => Target::Local(l),
                        None => continue,
                    },
                    _ => {
                        let key = txt(4);
                        a.std_hover.entry(key.clone()).or_insert_with(|| txt(5));
                        Target::Std(key)
                    }
                };
                a.refs.push((name_span, target));
            }
            "bind" => {
                let (Some(at), Some(ls)) = (a.map_span(sp(1)), a.map_span(sp(3))) else {
                    continue;
                };
                a.refs.push((at, Target::Local(ls)));
                // `x: T` (annotated binding or parameter) needs no hint.
                let rest = src[at.end..].trim_start();
                if !rest.starts_with(':') || rest.starts_with(":=") {
                    a.inferred.push(at);
                }
            }
            "meth" => {
                let tn = txt(1);
                let name = txt(7);
                let ns = a.map_span(sp(2));
                if f[6] == "1" && !name.starts_with('_') {
                    let detail = match ns {
                        Some(ns) => signature(src, ns.start),
                        None => txt(8),
                    };
                    a.members.entry(tn.clone()).or_default().push(Completion {
                        label: name.clone(),
                        kind: "method",
                        detail,
                    });
                }
                if let (Some(ns), Some(full)) = (ns, a.map_span(sp(4))) {
                    let sig = signature(src, ns.start);
                    a.def_names.insert(ns, name.clone());
                    a.def_kinds.insert(ns, "method");
                    a.def_hover
                        .insert(ns, hover_text(&sig, &doc_above(src, ns.start)));
                    a.symbols.push(Symbol {
                        name,
                        kind: "method",
                        span: ns,
                        full,
                        container: Some(tn),
                    });
                }
            }
            "local" => {
                let Some(ls) = a.map_span(sp(2)) else {
                    continue;
                };
                a.locals.insert(ls, (txt(4), txt(5)));
                if let Some(s) = scopes.iter_mut().find(|(k, _, _)| *k == n(1)) {
                    s.2.push(ls);
                }
            }
            "ety" => {
                let Some(at) = a.map_span(sp(1)) else {
                    continue;
                };
                a.expr_tys.push((at, txt(3)));
                let adt = txt(4);
                if !adt.is_empty() {
                    a.expr_type_names.push((at, adt));
                }
            }
            "field" => {
                let c = Completion {
                    label: txt(2),
                    kind: "field",
                    detail: txt(3),
                };
                let e = a.members.entry(txt(1)).or_default();
                if !e.contains(&c) {
                    e.push(c);
                }
            }
            "mref" => {
                let Some(call) = a.map_span(sp(1)) else {
                    continue;
                };
                let name = txt(3);
                let Some(ns) = method_name_span(src, call, &name) else {
                    continue;
                };
                let target = match f[4] {
                    "D" => match a.map_span(sp(5)) {
                        Some(d) => Target::Def(d),
                        None => continue,
                    },
                    _ => {
                        let key = txt(5);
                        a.std_hover.entry(key.clone()).or_insert_with(|| txt(6));
                        Target::Std(key)
                    }
                };
                a.refs.push((ns, target));
            }
            _ => {}
        }
    }
    a.scopes = scopes.into_iter().map(|(_, s, v)| (s, v)).collect();
    for v in a.members.values_mut() {
        v.sort();
        v.dedup_by(|x, y| x.label == y.label);
    }
    a.globals.sort();
    a.globals.dedup_by(|x, y| x.label == y.label);
    a.refs.sort_by_key(|(s, _)| (s.start, s.end));
    a.refs.dedup();
    a.symbols.sort_by_key(|s| s.span.start);
    a.expr_tys
        .sort_by_key(|(s, _)| (s.start, std::cmp::Reverse(s.end)));
    a
}

impl Analysis {
    /// Semantic tokens in `lo..hi` of [`Analysis::text`]: (span, index in
    /// [`TOKEN_TYPES`], is a declaration), sorted and non-overlapping.
    pub fn semantic_tokens(&self, lo: usize, hi: usize) -> Vec<(Span, u32, bool)> {
        let mut out: Vec<(Span, u32, bool)> = Vec::new();
        let inside = |s: &Span| s.start >= lo && s.end <= hi && s.start < s.end;
        for (s, k) in &self.def_kinds {
            if let (true, Some(t)) = (inside(s), token_type(k)) {
                out.push((*s, t, true));
            }
        }
        for (s, target) in &self.refs {
            if !inside(s) {
                continue;
            }
            let kind = match target {
                Target::Def(d) => self.def_kinds.get(d).copied(),
                Target::Local(_) => Some("local"),
                Target::Std(key) => {
                    let name = key.split('#').next().unwrap_or("");
                    let method = self.text.get(..s.start).is_some_and(|t| t.ends_with('.'));
                    if method {
                        Some("method")
                    } else if let Some(k) = self.pre_kinds.get(name) {
                        Some(*k)
                    } else if name.starts_with(char::is_uppercase) {
                        Some("type")
                    } else {
                        Some("fun")
                    }
                }
            };
            if let Some(t) = kind.and_then(token_type) {
                out.push((*s, t, matches!(target, Target::Local(l) if l == s)));
            }
        }
        out.sort_by_key(|(s, _, _)| (s.start, s.end));
        out.dedup_by(|b, a| b.0.start < a.0.end);
        out
    }

    /// Quick fixes for the diagnostics overlapping `lo..hi`.
    pub fn fixes(&self, lo: usize, hi: usize) -> Vec<Fix> {
        let mut out = Vec::new();
        for d in &self.diagnostics {
            if d.span.end < lo || d.span.start > hi {
                continue;
            }
            if let Some(missing) = d.message.strip_prefix("non-exhaustive match: ") {
                out.extend(self.match_fixes(d.span, missing));
            } else if let Some(name) = unresolved_name(&d.message) {
                out.extend(self.import_fixes(d.span.start, name));
            }
        }
        out
    }

    fn file_of(&self, at: usize) -> Option<(usize, usize)> {
        self.files
            .iter()
            .rfind(|(v, _, len)| *v <= at && at <= v + len)
            .map(|(v, _, len)| (*v, *len))
    }

    /// `import path::name` for each importable `name`, after the file's
    /// last top-level import (else at its top).
    fn import_fixes(&self, at: usize, name: &str) -> Vec<Fix> {
        let Some((v, len)) = self.file_of(at) else {
            return Vec::new();
        };
        let text = &self.text[v..v + len];
        let mut insert = 0;
        let mut off = 0;
        for line in text.split_inclusive('\n') {
            if line.starts_with("import ") {
                insert = off + line.len();
            }
            off += line.len();
        }
        let mut paths: Vec<&String> = self
            .importable
            .iter()
            .filter(|(n, _, _)| n == name)
            .map(|(_, _, p)| p)
            .collect();
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .map(|p| {
                let line = format!("import {p}::{name}\n");
                let (at, text) = if insert == 0 && !text.is_empty() {
                    (v, format!("{line}\n"))
                } else {
                    (v + insert, line)
                };
                Fix {
                    title: format!("Import `{p}::{name}`"),
                    edits: vec![(Span::new(at, at), text)],
                }
            })
            .collect()
    }

    /// Arms for the cases a `match` misses (`missing`: the rest of the
    /// diagnostic), and a catch-all `_` arm.
    fn match_fixes(&self, m: Span, missing: &str) -> Vec<Fix> {
        let Some(body) = self.text.get(m.start..m.end) else {
            return Vec::new();
        };
        let Some(close) = body.rfind('}') else {
            return Vec::new();
        };
        let close = m.start + close;
        let ls = self.text[..close].rfind('\n').map_or(0, |i| i + 1);
        let before = &self.text[ls..close];
        let (at, indent, lead) = if before.trim().is_empty() {
            (ls, format!("{before}\t"), String::new())
        } else {
            (close, String::new(), " ".to_string())
        };
        // `E::A => ...`: missing cases get the same `E::` prefix.
        let prefix = body
            .find('{')
            .and_then(|o| body[o + 1..].split("=>").next())
            .map(str::trim)
            .and_then(|pat| {
                let head = pat.split(['(', '{', ' ']).next()?;
                head.rfind("::").map(|i| head[..i + 2].to_string())
            })
            .unwrap_or_default();
        let arm = |pat: &str| {
            if indent.is_empty() {
                format!("{lead}{pat} => panic(\"todo\"), ")
            } else {
                format!("{indent}{pat} => panic(\"todo\"),\n")
            }
        };
        let mut out = Vec::new();
        let cases: Vec<&str> = missing
            .trim_end_matches(" not covered")
            .split(", ")
            .filter_map(|c| c.strip_prefix('`')?.strip_suffix('`'))
            .collect();
        if !cases.is_empty() && !missing.contains(" more") {
            let text: String = cases
                .iter()
                .map(|c| {
                    let qualified = c.starts_with(char::is_uppercase) && !c.contains("::");
                    arm(&if qualified {
                        format!("{prefix}{c}")
                    } else {
                        c.to_string()
                    })
                })
                .collect();
            out.push(Fix {
                title: "Add missing match arms".into(),
                edits: vec![(Span::new(at, at), text)],
            });
        }
        out.push(Fix {
            title: "Add a `_` arm".into(),
            edits: vec![(Span::new(at, at), arm("_"))],
        });
        out
    }

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

/// `x` in "cannot find value `x` in this scope" (and type, module).
fn unresolved_name(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix("cannot find ")?;
    if !rest.ends_with("` in this scope") {
        return None;
    }
    let start = rest.find('`')? + 1;
    let end = start + rest[start..].find('`')?;
    Some(&rest[start..end])
}
