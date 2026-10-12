//! Token definitions.

use crate::span::Span;
use std::fmt;

/// Reserved words of the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum Keyword {
    As,
    Break,
    Chan,
    Const,
    Continue,
    Defer,
    Dyn,
    Else,
    Enum,
    Extern,
    False,
    Fn,
    For,
    Go,
    If,
    Impl,
    Import,
    In,
    Let,
    Loop,
    Match,
    Mod,
    Move,
    Mut,
    Pub,
    Return,
    Select,
    SelfValue,
    SelfType,
    Static,
    Struct,
    Trait,
    True,
    Type,
    Unsafe,
    Use,
    Where,
    While,
    // Reserved for future use; always rejected by the parser.
    Async,
    Await,
    Macro,
    Yield,
    Try,
}

impl Keyword {
    /// All keywords paired with their source spelling.
    pub const ALL: &'static [(&'static str, Keyword)] = &[
        ("as", Keyword::As),
        ("break", Keyword::Break),
        ("chan", Keyword::Chan),
        ("const", Keyword::Const),
        ("continue", Keyword::Continue),
        ("defer", Keyword::Defer),
        ("dyn", Keyword::Dyn),
        ("else", Keyword::Else),
        ("enum", Keyword::Enum),
        ("extern", Keyword::Extern),
        ("false", Keyword::False),
        ("fun", Keyword::Fn),
        ("for", Keyword::For),
        ("go", Keyword::Go),
        ("if", Keyword::If),
        ("impl", Keyword::Impl),
        ("import", Keyword::Import),
        ("in", Keyword::In),
        ("let", Keyword::Let),
        ("loop", Keyword::Loop),
        ("match", Keyword::Match),
        ("mod", Keyword::Mod),
        ("move", Keyword::Move),
        ("mut", Keyword::Mut),
        ("pub", Keyword::Pub),
        ("return", Keyword::Return),
        ("select", Keyword::Select),
        ("self", Keyword::SelfValue),
        ("Self", Keyword::SelfType),
        ("static", Keyword::Static),
        ("struct", Keyword::Struct),
        ("trait", Keyword::Trait),
        ("true", Keyword::True),
        ("type", Keyword::Type),
        ("unsafe", Keyword::Unsafe),
        ("use", Keyword::Use),
        ("where", Keyword::Where),
        ("while", Keyword::While),
        ("async", Keyword::Async),
        ("await", Keyword::Await),
        ("macro", Keyword::Macro),
        ("yield", Keyword::Yield),
        ("try", Keyword::Try),
    ];

    /// Looks up a keyword by its spelling.
    pub fn lookup(s: &str) -> Option<Keyword> {
        Keyword::ALL
            .iter()
            .find(|(k, _)| *k == s)
            .map(|(_, kw)| *kw)
    }

    /// The source spelling of the keyword.
    pub fn as_str(self) -> &'static str {
        Keyword::ALL
            .iter()
            .find(|(_, k)| *k == self)
            .map(|(s, _)| *s)
            .unwrap()
    }

    /// Whether this keyword is reserved for future use.
    pub fn is_reserved(self) -> bool {
        matches!(
            self,
            Keyword::Async | Keyword::Await | Keyword::Macro | Keyword::Yield | Keyword::Try
        )
    }
}

/// One segment of a string literal.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    /// Literal text with escapes already processed.
    Text(String),
    /// An interpolated `{expr}`: the raw expression source and its byte offset in the file.
    Interp {
        /// Source text between the braces.
        src: String,
        /// Byte offset of `src` within the original file.
        offset: usize,
        /// The format spec after a top-level `:` (`{x:>8}` has `>8`), with its
        /// byte offset in the file.
        spec: Option<(String, usize)>,
    },
}

/// The kind of a token, with any payload.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum TokenKind {
    /// An identifier.
    Ident(String),
    /// A keyword.
    Keyword(Keyword),
    /// A lifetime or label, e.g. `'a` (stored without the quote).
    Lifetime(String),
    /// Integer literal with optional type suffix (`42u8`).
    Int {
        value: u128,
        suffix: Option<String>,
    },
    /// Float literal with optional suffix (`1e9f32`).
    Float {
        value: f64,
        text: String,
        suffix: Option<String>,
    },
    /// String literal, possibly interpolated.
    Str(Vec<StrPart>),
    /// Raw string literal `r"..."`/`r#"..."#` (no escapes, no interpolation).
    RawStr(String),
    /// Byte string literal `b"..."`/`br"..."` (type `&[u8]`).
    ByteStr(Vec<u8>),
    /// Character literal.
    Char(char),
    /// Statement terminator; `implicit` is true when inserted at a newline.
    Semi {
        implicit: bool,
    },

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    // Punctuation and operators
    Comma,
    Dot,
    DotDot,
    DotDotEq,
    Colon,
    ColonColon,
    ColonEq,
    Arrow,
    FatArrow,
    LArrow,
    Question,
    Hash,
    At,
    Underscore,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Bang,
    Amp,
    Pipe,
    AndAnd,
    OrOr,
    Shl,
    Shr,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    CaretEq,
    AmpEq,
    PipeEq,
    ShlEq,
    ShrEq,

    /// End of input.
    Eof,
}

impl TokenKind {
    /// Short human-readable description used in error messages.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Ident(s) => format!("identifier `{s}`"),
            TokenKind::Keyword(k) => format!("keyword `{}`", k.as_str()),
            TokenKind::Lifetime(l) => format!("lifetime `'{l}`"),
            TokenKind::Int { .. } => "integer literal".into(),
            TokenKind::Float { .. } => "float literal".into(),
            TokenKind::Str(_) | TokenKind::RawStr(_) => "string literal".into(),
            TokenKind::ByteStr(_) => "byte string literal".into(),
            TokenKind::Char(_) => "character literal".into(),
            TokenKind::Semi { implicit: true } => "newline".into(),
            TokenKind::Eof => "end of file".into(),
            other => format!("`{}`", other.punct_str().unwrap_or("?")),
        }
    }

    /// Source spelling for punctuation tokens.
    pub fn punct_str(&self) -> Option<&'static str> {
        use TokenKind::*;
        Some(match self {
            Semi { .. } => ";",
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            LBrace => "{",
            RBrace => "}",
            Comma => ",",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            Colon => ":",
            ColonColon => "::",
            ColonEq => ":=",
            Arrow => "->",
            FatArrow => "=>",
            LArrow => "<-",
            Question => "?",
            Hash => "#",
            At => "@",
            Underscore => "_",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            Caret => "^",
            Bang => "!",
            Amp => "&",
            Pipe => "|",
            AndAnd => "&&",
            OrOr => "||",
            Shl => "<<",
            Shr => ">>",
            Eq => "=",
            EqEq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            CaretEq => "^=",
            AmpEq => "&=",
            PipeEq => "|=",
            ShlEq => "<<=",
            ShrEq => ">>=",
            _ => return None,
        })
    }

    /// Whether a newline directly after this token ends the statement (Go's rule).
    pub fn ends_statement(&self) -> bool {
        use TokenKind::*;
        match self {
            Ident(_)
            | Lifetime(_)
            | Int { .. }
            | Float { .. }
            | Str(_)
            | RawStr(_)
            | ByteStr(_)
            | Char(_) => true,
            Keyword(k) => matches!(
                k,
                self::Keyword::Break
                    | self::Keyword::Continue
                    | self::Keyword::Return
                    | self::Keyword::True
                    | self::Keyword::False
                    | self::Keyword::SelfValue
                    | self::Keyword::SelfType
            ),
            RParen | RBracket | RBrace | Question | Underscore => true,
            _ => false,
        }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A token together with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Where it appears in the source.
    pub span: Span,
}
