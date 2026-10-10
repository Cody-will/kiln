//! The Kiln lexer: converts source text into tokens.
//!
//! Besides ordinary tokenization the lexer performs Go-style automatic
//! semicolon insertion: when a line ends after a token for which
//! [`TokenKind::ends_statement`] is true, an implicit [`TokenKind::Semi`] is
//! emitted. As an extension, no semicolon is inserted when the next line begins
//! with `.` (but not `..`), so method chains may be split across lines.

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Keyword, StrPart, Token, TokenKind};

/// Result of lexing a file.
#[derive(Debug, Clone, Default)]
pub struct Lexed {
    /// Tokens, always terminated by [`TokenKind::Eof`].
    pub tokens: Vec<Token>,
    /// `///` doc comments, in source order, with leading `///` and one space stripped.
    pub doc_comments: Vec<(Span, String)>,
    /// Lexical errors. Lexing always continues past errors.
    pub errors: Vec<Diagnostic>,
}

/// Lexes `src` starting at byte offset 0.
pub fn lex(src: &str) -> Lexed {
    lex_at(src, 0)
}

/// Lexes `src`, reporting spans shifted by `base` bytes. Used for interpolated
/// expressions inside string literals.
pub fn lex_at(src: &str, base: usize) -> Lexed {
    let mut l = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        base,
        out: Lexed::default(),
    };
    l.run();
    l.out
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    base: usize,
    out: Lexed,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn span(&self, start: usize) -> Span {
        Span::new(self.base + start, self.base + self.pos)
    }

    fn error(&mut self, msg: impl Into<String>, start: usize, end: usize) {
        self.out.errors.push(Diagnostic::error(
            msg,
            Span::new(self.base + start, self.base + end),
        ));
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start);
        self.out.tokens.push(Token { kind, span });
    }

    fn last_ends_statement(&self) -> bool {
        self.out
            .tokens
            .last()
            .is_some_and(|t| t.kind.ends_statement())
    }

    /// Called at a newline (at byte offset `at`): inserts a semicolon if needed.
    fn newline(&mut self, at: usize) {
        if !self.last_ends_statement() {
            return;
        }
        // Continuation: next non-blank line starting with a single `.`.
        let rest = &self.src[at..];
        let next = rest.trim_start_matches(|c: char| c.is_whitespace());
        if next.starts_with('.') && !next.starts_with("..") {
            return;
        }
        self.out.tokens.push(Token {
            kind: TokenKind::Semi { implicit: true },
            span: Span::new(self.base + at, self.base + at),
        });
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                '\n' => {
                    self.bump();
                    self.newline(start);
                }
                c if c.is_whitespace() => {
                    self.bump();
                }
                '/' if self.peek_at(1) == Some('/') => self.line_comment(),
                '/' if self.peek_at(1) == Some('*') => self.block_comment(),
                'r' if matches!(
                    (self.peek_at(1), self.peek_at(2)),
                    (Some('"'), _) | (Some('#'), Some('"' | '#'))
                ) =>
                {
                    self.raw_string()
                }
                c if is_ident_start(c) => self.ident(),
                '0'..='9' => self.number(),
                '"' => self.string(),
                '\'' => self.quote(),
                _ => self.punct(),
            }
        }
        let end = self.pos;
        self.newline(end);
        self.push(TokenKind::Eof, end);
    }

    fn line_comment(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
        let text = &self.src[start..self.pos];
        if let Some(doc) = text.strip_prefix("///") {
            if !doc.starts_with('/') {
                let doc = doc.strip_prefix(' ').unwrap_or(doc).to_string();
                let span = self.span(start);
                self.out.doc_comments.push((span, doc));
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1;
        let mut saw_newline = false;
        while depth > 0 {
            match self.peek() {
                None => {
                    self.error("unterminated block comment", start, start + 2);
                    return;
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.pos += 2;
                    depth += 1;
                }
                Some('*') if self.peek_at(1) == Some('/') => {
                    self.pos += 2;
                    depth -= 1;
                }
                Some(c) => {
                    saw_newline |= c == '\n';
                    self.bump();
                }
            }
        }
        // A block comment spanning lines acts like a newline (as in Go).
        if saw_newline {
            self.newline(self.pos);
        }
    }

    fn ident(&mut self) {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let text = &self.src[start..self.pos];
        let kind = if text == "_" {
            TokenKind::Underscore
        } else if let Some(kw) = Keyword::lookup(text) {
            TokenKind::Keyword(kw)
        } else {
            TokenKind::Ident(text.to_string())
        };
        self.push(kind, start);
    }

    fn eat_digits(&mut self, radix: u32) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c == '_' {
                self.bump();
            } else if c.is_digit(radix) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        s
    }

    fn suffix(&mut self) -> Option<String> {
        if self.peek().is_some_and(is_ident_start) {
            let s = self.pos;
            while self.peek().is_some_and(is_ident_continue) {
                self.bump();
            }
            Some(self.src[s..self.pos].to_string())
        } else {
            None
        }
    }

    fn number(&mut self) {
        let start = self.pos;
        let radix = match (self.peek(), self.peek_at(1)) {
            (Some('0'), Some('x')) => 16,
            (Some('0'), Some('o')) => 8,
            (Some('0'), Some('b')) => 2,
            _ => 10,
        };
        if radix != 10 {
            self.pos += 2;
            let digits = self.eat_digits(radix);
            if digits.is_empty() {
                self.error("missing digits after integer base prefix", start, self.pos);
            }
            let value = u128::from_str_radix(if digits.is_empty() { "0" } else { &digits }, radix)
                .unwrap_or_else(|_| {
                    self.error("integer literal is too large", start, self.pos);
                    0
                });
            let suffix = self.suffix();
            self.check_int_suffix(&suffix, start);
            self.push(TokenKind::Int { value, suffix }, start);
            return;
        }
        let mut text = self.eat_digits(10);
        let mut is_float = false;
        // Fraction: `1.5` but not `1..2` or `5.secs()`.
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            text.push('.');
            text.push_str(&self.eat_digits(10));
            is_float = true;
        }
        // Exponent.
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.pos;
            self.bump();
            let mut exp = String::from("e");
            if let Some(c @ ('+' | '-')) = self.peek() {
                self.bump();
                exp.push(c);
            }
            let digits = self.eat_digits(10);
            if digits.is_empty() {
                self.pos = save; // not an exponent; treat `e...` as suffix
            } else {
                text.push_str(&exp);
                text.push_str(&digits);
                is_float = true;
            }
        }
        let suffix = self.suffix();
        if is_float || matches!(suffix.as_deref(), Some("f32" | "f64")) {
            if let Some(s) = &suffix {
                if s != "f32" && s != "f64" {
                    self.error(
                        format!("invalid suffix `{s}` for float literal"),
                        start,
                        self.pos,
                    );
                }
            }
            let value = text.parse::<f64>().unwrap_or(0.0);
            self.push(
                TokenKind::Float {
                    value,
                    text,
                    suffix,
                },
                start,
            );
        } else {
            let value = text.parse::<u128>().unwrap_or_else(|_| {
                self.error("integer literal is too large", start, self.pos);
                0
            });
            self.check_int_suffix(&suffix, start);
            self.push(TokenKind::Int { value, suffix }, start);
        }
    }

    fn check_int_suffix(&mut self, suffix: &Option<String>, start: usize) {
        const OK: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        ];
        if let Some(s) = suffix {
            if !OK.contains(&s.as_str()) {
                self.error(
                    format!("invalid suffix `{s}` for integer literal"),
                    start,
                    self.pos,
                );
            }
        }
    }

    /// Parses an escape sequence after the backslash. Returns `None` on error.
    fn escape(&mut self, esc_start: usize) -> Option<char> {
        let c = self.bump()?;
        Some(match c {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            '{' => '{',
            '}' => '}',
            'x' => {
                let s = self.pos;
                for _ in 0..2 {
                    if self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                        self.bump();
                    }
                }
                match u8::from_str_radix(&self.src[s..self.pos], 16) {
                    Ok(v) if v <= 0x7f => v as char,
                    _ => {
                        self.error("invalid `\\x` escape (expected 00-7F)", esc_start, self.pos);
                        return None;
                    }
                }
            }
            'u' => {
                if self.peek() != Some('{') {
                    self.error("expected `{` after `\\u`", esc_start, self.pos);
                    return None;
                }
                self.bump();
                let s = self.pos;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_hexdigit() || c == '_')
                {
                    self.bump();
                }
                let hex = self.src[s..self.pos].replace('_', "");
                if self.peek() != Some('}') {
                    self.error("unterminated unicode escape", esc_start, self.pos);
                    return None;
                }
                self.bump();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => c,
                    None => {
                        self.error("invalid unicode escape", esc_start, self.pos);
                        return None;
                    }
                }
            }
            other => {
                self.error(format!("unknown escape `\\{other}`"), esc_start, self.pos);
                return None;
            }
        })
    }

    fn string(&mut self) {
        let start = self.pos;
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            let at = self.pos;
            match self.peek() {
                None => {
                    self.error("unterminated string literal", start, start + 1);
                    break;
                }
                Some('"') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    if let Some(c) = self.escape(at) {
                        text.push(c);
                    }
                }
                Some('{') if self.peek_at(1) == Some('{') => {
                    self.pos += 2;
                    text.push('{');
                }
                Some('}') if self.peek_at(1) == Some('}') => {
                    self.pos += 2;
                    text.push('}');
                }
                Some('{') => {
                    self.bump();
                    if !text.is_empty() {
                        parts.push(StrPart::Text(std::mem::take(&mut text)));
                    }
                    self.interpolation(at, &mut parts);
                }
                Some('}') => {
                    self.bump();
                    self.error("unmatched `}` in string literal", at, self.pos);
                    let _ = &mut text;
                }
                Some(c) => {
                    self.bump();
                    text.push(c);
                }
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(StrPart::Text(text));
        }
        self.push(TokenKind::Str(parts), start);
    }

    /// Scans an interpolation body after `{` up to the matching `}`.
    fn interpolation(&mut self, open: usize, parts: &mut Vec<StrPart>) {
        let s = self.pos;
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error(
                        "unterminated interpolation in string literal",
                        open,
                        open + 1,
                    );
                    return;
                }
                Some('{') => depth += 1,
                Some('}') if depth == 0 => break,
                Some('}') => depth -= 1,
                Some('"') => {
                    // Nested string inside the expression: skip it.
                    self.bump();
                    while let Some(c) = self.peek() {
                        if c == '\\' {
                            self.bump();
                        } else if c == '"' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => {}
            }
            self.bump();
        }
        let src = self.src[s..self.pos].to_string();
        self.bump(); // closing brace
        if src.trim().is_empty() {
            self.error(
                "empty interpolation `{}` (use `{{}}` for literal braces)",
                open,
                self.pos,
            );
            return;
        }
        parts.push(StrPart::Interp {
            src,
            offset: self.base + s,
        });
    }

    fn raw_string(&mut self) {
        let start = self.pos;
        self.bump(); // r
        let mut hashes = 0;
        while self.peek() == Some('#') {
            self.bump();
            hashes += 1;
        }
        if self.peek() != Some('"') {
            self.error("expected `\"` in raw string", start, self.pos);
            return;
        }
        self.bump();
        let body_start = self.pos;
        let closing: String = std::iter::once('"')
            .chain(std::iter::repeat_n('#', hashes))
            .collect();
        match self.src[self.pos..].find(&closing) {
            Some(i) => {
                let body = self.src[body_start..body_start + i].to_string();
                self.pos = body_start + i + closing.len();
                self.push(TokenKind::RawStr(body), start);
            }
            None => {
                self.pos = self.src.len();
                self.error("unterminated raw string literal", start, body_start);
                self.push(TokenKind::RawStr(String::new()), start);
            }
        }
    }

    /// Lexes a character literal (`'a'`) or a lifetime/label (`'a`).
    fn quote(&mut self) {
        let start = self.pos;
        self.bump();
        // Lifetime: ident chars not followed by a closing quote.
        if self.peek().is_some_and(is_ident_start) && self.peek_at(1) != Some('\'') {
            let s = self.pos;
            while self.peek().is_some_and(is_ident_continue) {
                self.bump();
            }
            if self.peek() != Some('\'') {
                let name = self.src[s..self.pos].to_string();
                self.push(TokenKind::Lifetime(name), start);
                return;
            }
            // Multi-char content followed by `'`: an invalid char literal.
            self.bump();
            self.error(
                "character literal may only contain one character",
                start,
                self.pos,
            );
            self.push(TokenKind::Char('\0'), start);
            return;
        }
        let c = match self.peek() {
            None | Some('\n') => {
                self.error("unterminated character literal", start, self.pos);
                return;
            }
            Some('\\') => {
                let at = self.pos;
                self.bump();
                self.escape(at).unwrap_or('\0')
            }
            Some('\'') => {
                self.bump();
                self.error("empty character literal", start, self.pos);
                self.push(TokenKind::Char('\0'), start);
                return;
            }
            Some(c) => {
                self.bump();
                c
            }
        };
        if self.peek() == Some('\'') {
            self.bump();
        } else {
            self.error("unterminated character literal", start, self.pos);
        }
        self.push(TokenKind::Char(c), start);
    }

    fn punct(&mut self) {
        use TokenKind::*;
        let start = self.pos;
        let rest = &self.bytes[self.pos..];
        let table: &[(&str, TokenKind)] = &[
            ("<<=", ShlEq),
            (">>=", ShrEq),
            ("..=", DotDotEq),
            ("::", ColonColon),
            (":=", ColonEq),
            ("->", Arrow),
            ("=>", FatArrow),
            ("<-", LArrow),
            ("..", DotDot),
            ("&&", AndAnd),
            ("||", OrOr),
            ("<<", Shl),
            (">>", Shr),
            ("==", EqEq),
            ("!=", Ne),
            ("<=", Le),
            (">=", Ge),
            ("+=", PlusEq),
            ("-=", MinusEq),
            ("*=", StarEq),
            ("/=", SlashEq),
            ("%=", PercentEq),
            ("^=", CaretEq),
            ("&=", AmpEq),
            ("|=", PipeEq),
            (";", Semi { implicit: false }),
            ("(", LParen),
            (")", RParen),
            ("[", LBracket),
            ("]", RBracket),
            ("{", LBrace),
            ("}", RBrace),
            (",", Comma),
            (".", Dot),
            (":", Colon),
            ("?", Question),
            ("#", Hash),
            ("@", At),
            ("+", Plus),
            ("-", Minus),
            ("*", Star),
            ("/", Slash),
            ("%", Percent),
            ("^", Caret),
            ("!", Bang),
            ("&", Amp),
            ("|", Pipe),
            ("=", Eq),
            ("<", Lt),
            (">", Gt),
        ];
        for (s, kind) in table {
            if rest.starts_with(s.as_bytes()) {
                self.pos += s.len();
                self.push(kind.clone(), start);
                return;
            }
        }
        let c = self.bump().unwrap();
        self.error(format!("unexpected character `{c}`"), start, self.pos);
    }
}
