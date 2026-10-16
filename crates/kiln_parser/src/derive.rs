//! Derive expansion: `#[derive(Json)]` (or `ToJson` / `FromJson` alone)
//! generates trait impls as Kiln source, parsed and appended next to the
//! type. Generated code lives in its own span range above
//! [`DERIVE_BASE`] and is registered as the source `<derive>`, so errors in
//! it point at readable text.

use crate::ast::{EnumDecl, Fields, File, GenericParam, Generics, Item, ItemKind, StructDecl};
use kiln_lexer::Diagnostic;
use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Spans of generated code start here (above all built-in sources).
pub const DERIVE_BASE: usize = 1 << 42;

static NEXT: AtomicUsize = AtomicUsize::new(DERIVE_BASE);

/// Reserves `n` bytes of generated-span space; returns its base.
pub(crate) fn reserve(n: usize) -> usize {
    NEXT.fetch_add(n, Ordering::Relaxed)
}

const J: &str = "std::encoding::json";

/// Expands derives in `file`, appending generated items and reporting
/// errors in generated code.
pub(crate) fn expand(file: &mut File, errors: &mut Vec<Diagnostic>) {
    expand_items(&mut file.items, errors);
}

fn expand_items(items: &mut Vec<Item>, errors: &mut Vec<Diagnostic>) {
    let mut code = String::new();
    for it in items.iter_mut() {
        if let ItemKind::Mod {
            items: Some(inner), ..
        } = &mut it.kind
        {
            expand_items(inner, errors);
        }
        let names = derives(it);
        let to = names.iter().any(|n| n == "Json" || n == "ToJson");
        let from = names.iter().any(|n| n == "Json" || n == "FromJson");
        if !to && !from {
            continue;
        }
        match &it.kind {
            ItemKind::Struct(s) => {
                if to {
                    code.push_str(&struct_to(s));
                }
                if from {
                    code.push_str(&struct_from(s));
                }
            }
            ItemKind::Enum(e) => {
                if to {
                    code.push_str(&enum_to(e));
                }
                if from {
                    code.push_str(&enum_from(e));
                }
            }
            _ => errors.push(Diagnostic::error(
                "`derive(Json)` applies to structs and enums",
                it.span,
            )),
        }
    }
    if code.is_empty() {
        return;
    }
    let text: &'static str = Box::leak(code.into_boxed_str());
    let base = NEXT.fetch_add(text.len() + 1, Ordering::Relaxed);
    kiln_lexer::register_source(base, "<derive>", text);
    let p = crate::parse_inner(text, base);
    errors.extend(p.errors);
    items.extend(p.file.items);
}

fn derives(item: &Item) -> Vec<String> {
    item.attrs
        .iter()
        .filter(|a| a.name.name == "derive")
        .flat_map(|a| {
            a.args
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `impl<'a, T: Bound> Trait for Name<'a, T>` header.
fn header(name: &str, g: &Generics, tr: &str) -> String {
    let mut params = vec![];
    let mut args = vec![];
    for p in &g.params {
        match p {
            GenericParam::Lifetime { name, .. } => {
                params.push(format!("'{}", name.name));
                args.push(format!("'{}", name.name));
            }
            GenericParam::Type { name, .. } => {
                params.push(format!("{}: {J}::{tr}", name.name));
                args.push(name.name.clone());
            }
            GenericParam::Const { name, .. } => {
                // Const parameters need their type; derive does not support them.
                args.push(name.name.clone());
            }
        }
    }
    let ps = if params.is_empty() {
        String::new()
    } else {
        format!("<{}>", params.join(", "))
    };
    let a = if args.is_empty() {
        String::new()
    } else {
        format!("<{}>", args.join(", "))
    };
    format!("impl{ps} {J}::{tr} for {name}{a}")
}

/// Statements building an object `var` from `(key, expr-of-a-reference)`.
fn object(var: &str, fields: &[(String, String)]) -> String {
    let mut s = format!("\t\t{var} mut := std::encoding::json::Map::new()\n");
    for (k, e) in fields {
        let _ = writeln!(
            s,
            "\t\t{var}.insert(\"{k}\".to_string(), {J}::ToJson::to_json({e}))"
        );
    }
    s
}

/// A Kiln string literal for `s` (braces doubled: Kiln strings interpolate).
fn lit(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '{' => o.push_str("{{"),
            '}' => o.push_str("}}"),
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Statements writing `fields` (raw JSON text pieces and value exprs) to `out`.
enum Piece {
    Text(String),
    Value(String),
}

fn emit(pieces: Vec<Piece>, indent: &str) -> String {
    // Merge adjacent text pieces.
    let mut merged: Vec<Piece> = vec![];
    for p in pieces {
        match (merged.last_mut(), p) {
            (Some(Piece::Text(a)), Piece::Text(b)) => a.push_str(&b),
            (_, p) => merged.push(p),
        }
    }
    let mut s = String::new();
    for p in merged {
        match p {
            Piece::Text(t)
                if t.chars().count() == 1 && !matches!(t.as_str(), "{" | "}" | "'" | "\\") =>
            {
                let _ = writeln!(s, "{indent}out.push_char('{t}')");
            }
            Piece::Text(t) => {
                let _ = writeln!(s, "{indent}out.push({})", lit(&t));
            }
            Piece::Value(e) => {
                let _ = writeln!(s, "{indent}{J}::ToJson::write_json({e}, out)");
            }
        }
    }
    s
}

/// Pieces of an object with `(key, expr)` members.
fn obj_pieces(fields: &[(String, String)]) -> Vec<Piece> {
    let mut v = vec![];
    if fields.is_empty() {
        v.push(Piece::Text("{}".into()));
        return v;
    }
    for (i, (k, e)) in fields.iter().enumerate() {
        let sep = if i == 0 { "{" } else { "," };
        v.push(Piece::Text(format!("{sep}\"{k}\":")));
        v.push(Piece::Value(e.clone()));
    }
    v.push(Piece::Text("}".into()));
    v
}

/// Pieces of an array of `exprs`.
fn arr_pieces(exprs: &[String]) -> Vec<Piece> {
    let mut v = vec![Piece::Text("[".into())];
    for (i, e) in exprs.iter().enumerate() {
        if i > 0 {
            v.push(Piece::Text(",".into()));
        }
        v.push(Piece::Value(e.clone()));
    }
    v.push(Piece::Text("]".into()));
    v
}

const WRITE_HEAD: &str = "\n\tfun write_json(&self, out &mut string) {\n";

fn struct_to(s: &StructDecl) -> String {
    let name = &s.name.name;
    let mut out = header(name, &s.generics, "ToJson");
    out.push_str(&format!(" {{\n\tfun to_json(&self) {J}::Value {{\n"));
    match &s.fields {
        Fields::Named(fs) => {
            let pairs: Vec<_> = fs
                .iter()
                .map(|f| (f.name.name.clone(), format!("&self.{}", f.name.name)))
                .collect();
            out.push_str(&object("m", &pairs));
            out.push_str(&format!("\t\treturn {J}::Value::Object(m)\n"));
        }
        Fields::Tuple(ts) if ts.len() == 1 => {
            out.push_str(&format!("\t\treturn {J}::ToJson::to_json(&self.0)\n"));
        }
        Fields::Tuple(ts) => {
            let items: Vec<_> = (0..ts.len())
                .map(|i| format!("{J}::ToJson::to_json(&self.{i})"))
                .collect();
            out.push_str(&format!(
                "\t\treturn {J}::Value::Array([{}].to_vec())\n",
                items.join(", ")
            ));
        }
        Fields::Unit => out.push_str(&format!("\t\treturn {J}::Value::Null\n")),
    }
    out.push_str("\t}\n");
    let pieces = match &s.fields {
        Fields::Named(fs) => obj_pieces(
            &fs.iter()
                .map(|f| (f.name.name.clone(), format!("&self.{}", f.name.name)))
                .collect::<Vec<_>>(),
        ),
        Fields::Tuple(ts) if ts.len() == 1 => vec![Piece::Value("&self.0".into())],
        Fields::Tuple(ts) => arr_pieces(
            &(0..ts.len())
                .map(|i| format!("&self.{i}"))
                .collect::<Vec<_>>(),
        ),
        Fields::Unit => vec![Piece::Text("null".into())],
    };
    out.push_str(WRITE_HEAD);
    out.push_str(&emit(pieces, "\t\t"));
    out.push_str("\t}\n}\n\n");
    out
}

/// A constructor expression from object `m` (named) or array `a` (tuple).
fn build(path: &str, fields: &Fields, src: &str, what: &str) -> (String, String) {
    match fields {
        Fields::Named(fs) => {
            let pre = format!("\t\tm := {J}::expect_object({src}, \"{what}\")?\n");
            let inits: Vec<_> = fs
                .iter()
                .map(|f| format!("{n}: {J}::field(m, \"{n}\")?", n = f.name.name))
                .collect();
            (pre, format!("{path} {{ {} }}", inits.join(", ")))
        }
        Fields::Tuple(ts) if ts.len() == 1 => {
            (String::new(), format!("{path}({J}::from_value({src})?)"))
        }
        Fields::Tuple(ts) => {
            let n = ts.len();
            let pre = format!("\t\ta := {J}::expect_array({src}, {n}, \"{what}\")?\n");
            let items: Vec<_> = (0..n).map(|i| format!("{J}::elem(a, {i})?")).collect();
            (pre, format!("{path}({})", items.join(", ")))
        }
        Fields::Unit => (String::new(), path.to_string()),
    }
}

fn struct_from(s: &StructDecl) -> String {
    let name = &s.name.name;
    let mut out = header(name, &s.generics, "FromJson");
    out.push_str(&format!(
        " {{\n\tfun from_json(v &{J}::Value) result<Self, {J}::Error> {{\n"
    ));
    if let Fields::Unit = s.fields {
        out.push_str("\t\tif !v.is_null() {\n");
        out.push_str(&format!(
            "\t\t\treturn Err({J}::Error::mismatch(\"null for {name}\", v))\n\t\t}}\n"
        ));
    }
    let (pre, ctor) = build(name, &s.fields, "v", name);
    out.push_str(&pre);
    out.push_str(&format!("\t\treturn Ok({ctor})\n\t}}\n"));
    if let Fields::Named(fs) = &s.fields {
        out.push_str(&read_named(
            name,
            fs.iter().map(|f| f.name.name.as_str()).collect(),
        ));
    }
    out.push_str("}\n\n");
    out
}

/// `read_json` decoding a struct with named fields straight from the text.
fn read_named(name: &str, fields: Vec<&str>) -> String {
    let mut o =
        format!("\n\tfun read_json<'p>(p &mut {J}::Parser<'p>) result<Self, {J}::Error> {{\n");
    o.push_str("\t\tif !p.at_object() {\n\t\t\tv := p.read_value()?\n\t\t\treturn Self::from_json(&v)\n\t\t}\n");
    o.push_str("\t\tp.begin()?\n");
    for i in 0..fields.len() {
        let _ = writeln!(o, "\t\tf{i} mut := None");
    }
    o.push_str("\t\tfirst mut := true\n\t\twhile p.next_key(first)? {\n\t\t\tfirst = false\n");
    for (i, f) in fields.iter().enumerate() {
        let kw = if i == 0 { "if" } else { "} else if" };
        let _ = writeln!(
            o,
            "\t\t\t{kw} p.is_key(\"{f}\") {{\n\t\t\t\tf{i} = Some({J}::FromJson::read_json(p)?)"
        );
    }
    if fields.is_empty() {
        o.push_str("\t\t\tp.skip()?\n");
    } else {
        o.push_str("\t\t\t} else {\n\t\t\t\tp.skip()?\n\t\t\t}\n");
    }
    o.push_str("\t\t}\n");
    let inits: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{f}: {J}::finish_field(f{i}, \"{f}\")?"))
        .collect();
    let _ = writeln!(o, "\t\treturn Ok({name} {{ {} }})\n\t}}", inits.join(", "));
    o
}

fn enum_to(e: &EnumDecl) -> String {
    let name = &e.name.name;
    let mut out = header(name, &e.generics, "ToJson");
    out.push_str(&format!(
        " {{\n\tfun to_json(&self) {J}::Value {{\n\t\tmatch self {{\n"
    ));
    for v in &e.variants {
        let vn = &v.name.name;
        match &v.fields {
            Fields::Unit => {
                let _ = writeln!(
                    out,
                    "\t\t\t{name}::{vn} => return {J}::Value::String(\"{vn}\".to_string()),"
                );
            }
            Fields::Tuple(ts) => {
                let binds: Vec<_> = (0..ts.len()).map(|i| format!("x{i}")).collect();
                let payload = if ts.len() == 1 {
                    format!("{J}::ToJson::to_json(x0)")
                } else {
                    let items: Vec<_> = binds
                        .iter()
                        .map(|b| format!("{J}::ToJson::to_json({b})"))
                        .collect();
                    format!("{J}::Value::Array([{}].to_vec())", items.join(", "))
                };
                let _ = writeln!(
                    out,
                    "\t\t\t{name}::{vn}({}) => {{\n\t\tp := {payload}",
                    binds.join(", ")
                );
                out.push_str(&tagged(vn));
            }
            Fields::Named(fs) => {
                let binds: Vec<_> = fs.iter().map(|f| f.name.name.clone()).collect();
                let _ = writeln!(out, "\t\t\t{name}::{vn} {{ {} }} => {{", binds.join(", "));
                let pairs: Vec<_> = binds.iter().map(|b| (b.clone(), b.clone())).collect();
                out.push_str(&object("inner", &pairs));
                let _ = writeln!(out, "\t\tp := {J}::Value::Object(inner)");
                out.push_str(&tagged(vn));
            }
        }
    }
    out.push_str("\t\t}\n\t}\n");
    out.push_str(WRITE_HEAD);
    out.push_str("\t\tmatch self {\n");
    for v in &e.variants {
        let vn = &v.name.name;
        let head = format!("{{\"{vn}\":");
        let (pat, mut pieces) = match &v.fields {
            Fields::Unit => (
                format!("{name}::{vn}"),
                vec![Piece::Text(format!("\"{vn}\""))],
            ),
            Fields::Tuple(ts) => {
                let binds: Vec<_> = (0..ts.len()).map(|i| format!("x{i}")).collect();
                let mut p = vec![Piece::Text(head.clone())];
                if ts.len() == 1 {
                    p.push(Piece::Value("x0".into()));
                } else {
                    p.extend(arr_pieces(&binds));
                }
                p.push(Piece::Text("}".into()));
                (format!("{name}::{vn}({})", binds.join(", ")), p)
            }
            Fields::Named(fs) => {
                let binds: Vec<_> = fs.iter().map(|f| f.name.name.clone()).collect();
                let pairs: Vec<_> = binds.iter().map(|b| (b.clone(), b.clone())).collect();
                let mut p = vec![Piece::Text(head.clone())];
                p.extend(obj_pieces(&pairs));
                p.push(Piece::Text("}".into()));
                (format!("{name}::{vn} {{ {} }}", binds.join(", ")), p)
            }
        };
        let body = emit(std::mem::take(&mut pieces), "\t\t\t\t");
        let _ = writeln!(out, "\t\t\t{pat} => {{\n{body}\t\t\t}}");
    }
    out.push_str("\t\t}\n\t}\n}\n\n");
    out
}

/// Returns `{"vn": p}` and closes the match arm.
fn tagged(vn: &str) -> String {
    let mut s = object("o", &[]);
    let _ = writeln!(s, "\t\to.insert(\"{vn}\".to_string(), p)");
    let _ = writeln!(s, "\t\treturn {J}::Value::Object(o)\n\t\t\t}}");
    s
}

fn enum_from(e: &EnumDecl) -> String {
    let name = &e.name.name;
    let mut out = header(name, &e.generics, "FromJson");
    out.push_str(&format!(
        " {{\n\tfun from_json(v &{J}::Value) result<Self, {J}::Error> {{\n"
    ));
    let _ = writeln!(out, "\t\t(name, p) := {J}::variant(v, \"{name}\")?");
    for v in &e.variants {
        let vn = &v.name.name;
        let path = format!("{name}::{vn}");
        let _ = writeln!(out, "\t\tif name == \"{vn}\" {{");
        if !matches!(v.fields, Fields::Unit) {
            let _ = writeln!(out, "\t\tx := {J}::payload(p, \"{vn}\")?");
        }
        let (pre, ctor) = build(&path, &v.fields, "x", &path);
        out.push_str(&pre);
        let _ = writeln!(out, "\t\t\treturn Ok({ctor})\n\t\t}}");
    }
    let _ = writeln!(
        out,
        "\t\treturn Err({J}::unknown_variant(name, \"{name}\"))\n\t}}\n}}\n"
    );
    out
}
