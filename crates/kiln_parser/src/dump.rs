//! Canonical AST dump, used to diff the self-hosted parser
//! (`compiler/src/parser.kn`) against this one.
//!
//! The format is the pretty `Debug` output of [`ast::File`] (before derive
//! expansion), with every `Span { start: a, end: b }` collapsed onto one
//! line as `a..b`, followed by one `error <start>..<end>: <message>` line per
//! diagnostic.

use crate::{ast, parse_inner};

/// Returns the canonical dump of `src`.
pub fn dump(src: &str) -> String {
    let p = parse_inner(src, 0);
    dump_file(&p.file, &p.errors)
}

/// [`dump`] after derive expansion (generated items appended). Generated
/// spans start at [`crate::DERIVE_BASE`] in a fresh process, so this is only
/// reproducible as the first parse in a process (`kiln parse --dump-expanded`).
pub fn dump_expanded(src: &str) -> String {
    let p = crate::parse(src);
    dump_file(&p.file, &p.errors)
}

/// The derive dump of `src`: each generated source text after a
/// `-- derive` line, then `error <start>..<end>: <message>` lines for
/// derive errors on the user's items.
pub fn dump_derive(src: &str) -> String {
    let p = parse_inner(src, 0);
    let (texts, errors) = crate::derive::generated(&p.file);
    let mut out = String::new();
    for t in texts {
        out.push_str("-- derive\n");
        out.push_str(&t);
    }
    for e in errors {
        out.push_str(&format!(
            "error {}..{}: {}\n",
            e.span.start, e.span.end, e.message
        ));
    }
    out
}

fn dump_file(file: &ast::File, errors: &[kiln_lexer::Diagnostic]) -> String {
    let raw = format!("{file:#?}");
    let mut out = String::with_capacity(raw.len());
    let mut lines = raw.lines();
    while let Some(line) = lines.next() {
        if let Some(head) = line.strip_suffix("Span {") {
            let start = field(lines.next(), "start");
            let end = field(lines.next(), "end");
            let close = lines.next().unwrap_or("").trim_start();
            out.push_str(&format!("{head}{start}..{end}{}\n", &close[1..]));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    for e in errors {
        out.push_str(&format!(
            "error {}..{}: {}\n",
            e.span.start, e.span.end, e.message
        ));
    }
    out
}

fn field(line: Option<&str>, name: &str) -> String {
    let l = line.unwrap_or("").trim();
    l.strip_prefix(name)
        .and_then(|r| r.strip_prefix(": "))
        .map(|r| r.trim_end_matches(',').to_string())
        .unwrap_or_else(|| panic!("unexpected Span layout: {l:?}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn spans_collapse_and_errors_follow() {
        let d = super::dump("fun main() {\n    x(\n}\n");
        assert!(d.contains("span: 4..8,"), "{d}");
        assert!(!d.contains("Span {"));
        assert!(d.lines().any(|l| l.starts_with("error ")), "{d}");
    }
}
