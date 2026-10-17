//! A canonical, line-per-item text form of a [`Lexed`] result.
//!
//! The self-hosted lexer (`compiler/src/lexer.kn`) prints the same format,
//! and `tests/lexer_diff.rs` compares the two over every `.kn` file in the
//! repository. The format:
//!
//! - `tok <start> <end> <kind> [payload...]`, one per token;
//! - `doc <start> <end> <text>`, one per `///` doc comment;
//! - `err <start> <end> <message>`, one per lexical error.
//!
//! Text is escaped by [`esc`]: bytes outside printable ASCII, and `\`,
//! become `\xHH`. Float values are given by their source text only.

use crate::lexer::Lexed;
use crate::token::{StrPart, TokenKind};
use std::fmt::Write;

/// Escapes `s` so it fits on one line with no spaces.
pub fn esc(s: &[u8]) -> String {
    let mut o = String::new();
    for &b in s {
        if b > 32 && b < 127 && b != b'\\' {
            o.push(b as char);
        } else {
            let _ = write!(o, "\\x{b:02x}");
        }
    }
    if o.is_empty() {
        o.push('-');
    }
    o
}

fn opt(s: &Option<String>) -> String {
    match s {
        Some(s) => esc(s.as_bytes()),
        None => "-".into(),
    }
}

/// The dump of `l`.
pub fn dump(l: &Lexed) -> String {
    let mut o = String::new();
    for t in &l.tokens {
        let _ = write!(o, "tok {} {} ", t.span.start, t.span.end);
        let _ = match &t.kind {
            TokenKind::Ident(s) => write!(o, "ident {}", esc(s.as_bytes())),
            TokenKind::Keyword(k) => write!(o, "kw {}", k.as_str()),
            TokenKind::Lifetime(s) => write!(o, "life {}", esc(s.as_bytes())),
            TokenKind::Int { value, suffix } => write!(o, "int {value} {}", opt(suffix)),
            TokenKind::Float { text, suffix, .. } => {
                write!(o, "float {} {}", esc(text.as_bytes()), opt(suffix))
            }
            TokenKind::Str(parts) => {
                o.push_str("str");
                for p in parts {
                    let _ = match p {
                        StrPart::Text(s) => write!(o, " T{}", esc(s.as_bytes())),
                        StrPart::Interp { src, offset, spec } => {
                            let _ = write!(o, " I{}@{offset}", esc(src.as_bytes()));
                            match spec {
                                Some((s, at)) => write!(o, ":{}@{at}", esc(s.as_bytes())),
                                None => Ok(()),
                            }
                        }
                    };
                }
                Ok(())
            }
            TokenKind::RawStr(s) => write!(o, "raw {}", esc(s.as_bytes())),
            TokenKind::ByteStr(b) => write!(o, "bytes {}", esc(b)),
            TokenKind::Char(c) => write!(o, "char {}", *c as u32),
            TokenKind::Semi { implicit } => write!(o, "semi {}", u8::from(*implicit)),
            TokenKind::Eof => write!(o, "eof"),
            k => write!(o, "p {}", k.punct_str().unwrap_or("?")),
        };
        o.push('\n');
    }
    for (s, text) in &l.doc_comments {
        let _ = writeln!(o, "doc {} {} {}", s.start, s.end, esc(text.as_bytes()));
    }
    for e in &l.errors {
        let _ = writeln!(
            o,
            "err {} {} {}",
            e.span.start,
            e.span.end,
            esc(e.message.as_bytes())
        );
    }
    o
}
