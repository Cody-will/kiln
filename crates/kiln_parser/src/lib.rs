//! Parser and abstract syntax tree for the Kiln programming language.
//!
//! [`parse`] lexes and parses a whole source file, returning the [`ast::File`]
//! together with all lexical and syntax diagnostics. The parser recovers at
//! item and statement boundaries, so one mistake does not hide the rest of the file.

pub mod ast;
mod parser;

use kiln_lexer::{lex, Diagnostic};

/// Result of parsing a source file.
#[derive(Debug, Clone)]
pub struct Parse {
    /// The syntax tree (possibly partial when there are errors).
    pub file: ast::File,
    /// Lexer and parser diagnostics, in the order produced.
    pub errors: Vec<Diagnostic>,
}

/// Lexes and parses a complete Kiln source file.
pub fn parse(src: &str) -> Parse {
    parse_at(src, 0)
}

/// Parses `src` as if it started at byte offset `base` (all spans are shifted
/// by `base`). Used for built-in sources, which live above
/// [`kiln_lexer::BUILTIN_BASE`].
pub fn parse_at(src: &str, base: usize) -> Parse {
    let lexed = kiln_lexer::lex_at(src, base);
    let mut p = parser::Parser::new(src, lexed.tokens, lexed.doc_comments);
    p.base = base;
    let file = p.parse_file();
    let mut errors = lexed.errors;
    errors.extend(p.errors);
    Parse { file, errors }
}

/// Parses a single expression (for tests and tooling).
pub fn parse_expr(src: &str) -> (Option<ast::Expr>, Vec<Diagnostic>) {
    let lexed = lex(src);
    let mut p = parser::Parser::new(src, lexed.tokens, Vec::new());
    let e = p.parse_expr().ok();
    let mut errors = lexed.errors;
    errors.extend(p.errors);
    (e, errors)
}

#[cfg(test)]
mod tests;
