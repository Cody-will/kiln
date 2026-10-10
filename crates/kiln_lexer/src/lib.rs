//! Lexer for the Kiln programming language.
//!
//! The entry point is [`lex`], which turns source text into a [`Lexed`]
//! containing tokens, doc comments, and diagnostics. See `docs/SPEC.md` §2 for
//! the lexical grammar.

pub mod diagnostic;
mod lexer;
pub mod span;
pub mod token;

pub use diagnostic::{Diagnostic, Severity};
pub use lexer::{lex, lex_at, Lexed};
pub use span::{LineIndex, Span};
pub use token::{Keyword, StrPart, Token, TokenKind};

#[cfg(test)]
mod tests;
