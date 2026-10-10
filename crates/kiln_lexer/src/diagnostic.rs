//! Compiler diagnostics shared by all front-end phases.

use crate::span::{LineIndex, Span};
use std::fmt::Write;

/// How severe a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A hard error; compilation cannot succeed.
    Error,
    /// A warning; compilation continues.
    Warning,
}

/// A message attached to a source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity of the diagnostic.
    pub severity: Severity,
    /// Primary human-readable message.
    pub message: String,
    /// Location the diagnostic points at.
    pub span: Span,
    /// Optional extra help text.
    pub help: Option<String>,
    /// Secondary notes, optionally pointing at another location
    /// (e.g. "value moved here").
    pub notes: Vec<(String, Option<Span>)>,
}

impl Diagnostic {
    /// Creates an error diagnostic.
    pub fn error(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            span,
            help: None,
            notes: Vec::new(),
        }
    }

    /// Attaches help text.
    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// Attaches a note pointing at `span`.
    pub fn with_note(mut self, msg: impl Into<String>, span: Span) -> Diagnostic {
        self.notes.push((msg.into(), Some(span)));
        self
    }

    /// Attaches a note without a location.
    pub fn with_plain_note(mut self, msg: impl Into<String>) -> Diagnostic {
        self.notes.push((msg.into(), None));
        self
    }

    /// Renders the diagnostic in a rustc-like format with a source snippet.
    pub fn render(&self, file: &str, src: &str) -> String {
        let idx = LineIndex::new(src);
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut out = String::new();
        let _ = writeln!(out, "{sev}: {}", self.message);
        let gutter = snippet(&mut out, &idx, file, src, self.span);
        for (msg, span) in &self.notes {
            match span {
                Some(sp) => {
                    let _ = writeln!(out, "note: {msg}");
                    snippet(&mut out, &idx, file, src, *sp);
                }
                None => {
                    let _ = writeln!(out, "{:gutter$} = note: {msg}", "");
                }
            }
        }
        if let Some(h) = &self.help {
            let _ = writeln!(out, "{:gutter$} = help: {h}", "");
        }
        out
    }
}

/// Writes a `--> file:line:col` header and the source line with a caret
/// under `span`. Returns the gutter width.
fn snippet(out: &mut String, idx: &LineIndex, file: &str, src: &str, span: Span) -> usize {
    let (line, col) = idx.line_col(span.start.min(src.len()));
    let (ls, le) = idx.line_range(line, src);
    let text = &src[ls..le];
    let gutter = line.to_string().len();
    let _ = writeln!(out, "{:gutter$}--> {file}:{line}:{col}", "");
    let _ = writeln!(out, "{:gutter$} |", "");
    let _ = writeln!(out, "{line} | {text}");
    let width = span
        .len()
        .clamp(1, (le - ls + 1).saturating_sub(col - 1).max(1));
    let pad: String = text
        .chars()
        .take(col - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let _ = writeln!(out, "{:gutter$} | {pad}{}", "", "^".repeat(width));
    gutter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_snippet() {
        let src = "fn main() {\n\tx := $\n}\n";
        let d = Diagnostic::error("unexpected character `$`", Span::new(18, 19));
        let r = d.render("a.kn", src);
        assert!(r.contains("error: unexpected character `$`"));
        assert!(r.contains("a.kn:2:7"));
        assert!(r.contains("2 | \tx := $"));
        assert!(r.contains("\t     ^"));
    }

    #[test]
    fn renders_notes() {
        let src = "a\nbb\n";
        let d = Diagnostic::error("use of moved value", Span::new(2, 4))
            .with_note("value moved here", Span::new(0, 1))
            .with_plain_note("plain")
            .with_help("clone it");
        let r = d.render("f.kn", src);
        assert!(r.contains("note: value moved here\n --> f.kn:1:1"), "{r}");
        assert!(r.contains("= note: plain"));
        assert!(r.find("note: value moved").unwrap() < r.find("help:").unwrap());
    }
}
