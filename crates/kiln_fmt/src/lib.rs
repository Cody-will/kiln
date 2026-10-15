//! The canonical Kiln formatter: `kiln fmt` (see SPEC.md section 10).
//!
//! The formatter works on the token stream plus the comments between tokens,
//! so it never loses a comment and never reorders code. It keeps the line
//! breaks the author chose (as gofmt does) and normalizes everything else:
//!
//! * indentation: one tab per open bracket level, plus one for a continued
//!   line (a line that starts with `.` or follows a line ending in an
//!   operator);
//! * spacing between tokens on a line (operators, commas, braces, calls);
//! * trailing commas: added when a list ends on its own line, removed when
//!   it closes on the same line;
//! * semicolons that a newline already provides are removed;
//! * at most one blank line in a row, none just inside brackets, and one
//!   between top-level items;
//! * consecutive `import` lines are sorted and deduplicated;
//! * `//comment` becomes `// comment`.
//!
//! Where spacing is ambiguous from tokens alone (`<` as generics or as
//! less-than, `|` for closures, `..` ranges), the author's choice of space or
//! no space is kept, which keeps the output stable.
//!
//! [`format`] checks its own work: the formatted text must lex to the same
//! tokens (ignoring commas and semicolons it was allowed to change) and
//! comments as the input, or it returns an error instead of a wrong result.

use kiln_lexer::{Diagnostic, Keyword as K, Token, TokenKind as T};

/// Why a file could not be formatted.
#[derive(Debug)]
pub enum Error {
    /// The file has syntax errors; it is left unchanged.
    Syntax(Vec<Diagnostic>),
    /// The formatter produced output that does not match the input (a bug).
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Syntax(d) => write!(f, "{} syntax error(s)", d.len()),
            Error::Internal(m) => write!(f, "internal formatter error: {m}"),
        }
    }
}

/// Formats a Kiln source file.
pub fn format(src: &str) -> Result<String, Error> {
    let parsed = kiln_parser::parse(src);
    if !parsed.errors.is_empty() {
        return Err(Error::Syntax(parsed.errors));
    }
    let out = format_tokens(src);
    verify(src, &out)?;
    Ok(out)
}

/// One thing on a line: a token (index into the token list) or a comment.
#[derive(Clone, Debug)]
enum Elem {
    Tok(usize),
    Comment(String),
}

#[derive(Debug, Default)]
struct Line {
    elems: Vec<Elem>,
    blank_before: bool,
}

/// Comments in a gap between tokens, with the number of newlines before each
/// (and after the last, in `trailing_newlines`).
struct Gap {
    items: Vec<(usize, String)>,
    trailing_newlines: usize,
}

fn scan_gap(text: &str) -> Gap {
    let b = text.as_bytes();
    let mut i = 0;
    let mut nl = 0;
    let mut items = Vec::new();
    while i < b.len() {
        if b[i] == b'\n' {
            nl += 1;
            i += 1;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            items.push((nl, text[start..i].trim_end().to_string()));
            nl = 0;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            items.push((nl, text[start..i].to_string()));
            nl = 0;
        } else {
            i += 1;
        }
    }
    Gap {
        items,
        trailing_newlines: nl,
    }
}

fn normalize_comment(c: &str) -> String {
    if let Some(rest) = c.strip_prefix("//") {
        if rest.starts_with(|ch: char| ch.is_alphanumeric()) {
            return format!("// {rest}");
        }
    }
    c.to_string()
}

fn is_open(k: &T) -> bool {
    matches!(k, T::LParen | T::LBracket | T::LBrace)
}

fn is_close(k: &T) -> bool {
    matches!(k, T::RParen | T::RBracket | T::RBrace)
}

/// Binary-only operators: always a space on each side.
fn is_binary_op(k: &T) -> bool {
    matches!(
        k,
        T::Plus
            | T::Slash
            | T::Percent
            | T::Caret
            | T::OrOr
            | T::Eq
            | T::EqEq
            | T::Ne
            | T::Le
            | T::Ge
            | T::ColonEq
            | T::Arrow
            | T::FatArrow
            | T::PlusEq
            | T::MinusEq
            | T::StarEq
            | T::SlashEq
            | T::PercentEq
            | T::CaretEq
            | T::AmpEq
            | T::PipeEq
            | T::ShlEq
            | T::ShrEq
    )
}

/// Operators that are prefix when they start an operand: `-x`, `*p`, `&v`,
/// `&&v`, `<-ch`.
fn is_maybe_unary(k: &T) -> bool {
    matches!(k, T::Minus | T::Star | T::Amp | T::AndAnd | T::LArrow)
}

/// Whether a token is a word or literal (an operand's last token).
fn is_operand_end(k: &T) -> bool {
    match k {
        T::Keyword(kw) => matches!(kw, K::SelfValue | K::SelfType | K::True | K::False),
        _ => k.ends_statement() && !matches!(k, T::RBrace),
    }
}

fn is_word(k: &T) -> bool {
    matches!(
        k,
        T::Ident(_)
            | T::Keyword(_)
            | T::Lifetime(_)
            | T::Int { .. }
            | T::Float { .. }
            | T::Str(_)
            | T::RawStr(_)
            | T::ByteStr(_)
            | T::Char(_)
            | T::Underscore
    )
}

struct Fmt<'a> {
    src: &'a str,
    toks: Vec<Token>,
    /// For each token: whether an operator at this index is prefix (unary).
    unary: Vec<bool>,
    /// For each opening bracket: whether its contents are a comma list.
    list: Vec<bool>,
    /// For each opening bracket: the index of its closing bracket.
    close_of: Vec<Option<usize>>,
    /// Commas to drop and to add (after token i).
    drop: Vec<bool>,
    add_comma: Vec<bool>,
}

fn format_tokens(src: &str) -> String {
    let lexed = kiln_lexer::lex(src);
    let toks: Vec<Token> = lexed
        .tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, T::Semi { implicit: true } | T::Eof))
        .collect();
    let n = toks.len();
    let mut f = Fmt {
        src,
        unary: vec![false; n],
        list: vec![false; n],
        close_of: vec![None; n],
        drop: vec![false; n],
        add_comma: vec![false; n],
        toks,
    };
    f.analyze();
    let lines = f.build_lines();
    f.render(lines)
}

impl Fmt<'_> {
    fn text(&self, i: usize) -> &str {
        let s = self.toks[i].span;
        &self.src[s.start..s.end]
    }

    fn k(&self, i: usize) -> &T {
        &self.toks[i].kind
    }

    /// Whether there is a line break between tokens `i` and `i + 1`.
    fn newline_after(&self, i: usize) -> bool {
        let a = self.toks[i].span.end;
        let b = self
            .toks
            .get(i + 1)
            .map_or(self.src.len(), |t| t.span.start);
        self.src[a..b].contains('\n')
    }

    fn space_in_source(&self, i: usize) -> bool {
        let a = self.toks[i].span.end;
        let b = self.toks[i + 1].span.start;
        b > a
    }

    fn analyze(&mut self) {
        let n = self.toks.len();
        // Unary operators.
        for i in 0..n {
            if is_maybe_unary(self.k(i)) {
                // `x &T`, `s *const u8`: a name followed by a type (a space
                // before the operator and none after) is a prefix use too.
                let glued_type = i > 0
                    && i + 1 < n
                    && matches!(self.k(i - 1), T::Ident(_) | T::RParen | T::Gt | T::Shr)
                    && matches!(self.k(i), T::Amp | T::AndAnd | T::Star | T::LArrow)
                    && self.space_in_source(i - 1)
                    && !self.space_in_source(i);
                self.unary[i] = i == 0
                    || !is_operand_end(self.k(i - 1))
                    || self.prev_is_line_end(i)
                    || glued_type;
            }
            // `!` is always prefix (`!=` is its own token); `x!` is a macro.
        }
        // Bracket matching.
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..n {
            let k = self.k(i).clone();
            if is_open(&k) {
                stack.push(i);
            } else if is_close(&k) {
                if let Some(o) = stack.pop() {
                    self.close_of[o] = Some(i);
                }
            }
        }
        for o in 0..n {
            let Some(c) = self.close_of[o] else { continue };
            self.list[o] = self.is_list(o, c);
            if !self.list[o] || c == o + 1 {
                continue;
            }
            let last = c - 1;
            let multiline = self.newline_after(last) || self.ends_with_comment_line(last, c);
            let has_comma = matches!(self.k(last), T::Comma);
            if multiline && !has_comma && !is_open(self.k(last)) {
                self.add_comma[last] = true;
            } else if !multiline && has_comma {
                // `(x,)` is a one-element tuple: keep its comma.
                let tuple1 = matches!(self.k(o), T::LParen) && self.top_level_commas(o, c) == 1;
                if !tuple1 {
                    self.drop[last] = true;
                }
            }
        }
        // Explicit semicolons a newline (or a closing brace) already provides.
        for i in 0..n {
            if matches!(self.k(i), T::Semi { implicit: false }) {
                let next_close = i + 1 >= n || matches!(self.k(i + 1), T::RBrace);
                if self.newline_after(i) || next_close {
                    self.drop[i] = true;
                }
            }
        }
    }

    /// Whether only comments separate the last element from the closing
    /// bracket on a later line (`a // note\n)`).
    fn ends_with_comment_line(&self, last: usize, close: usize) -> bool {
        let a = self.toks[last].span.end;
        let b = self.toks[close].span.start;
        self.src[a..b].contains('\n')
    }

    fn prev_is_line_end(&self, i: usize) -> bool {
        i > 0 && self.newline_after(i - 1)
    }

    fn top_level_commas(&self, o: usize, c: usize) -> usize {
        let mut depth = 0;
        let mut n = 0;
        for i in o + 1..c {
            let k = self.k(i);
            if is_open(k) {
                depth += 1;
            } else if is_close(k) {
                depth -= 1;
            } else if depth == 0 && matches!(k, T::Comma) {
                n += 1;
            }
        }
        n
    }

    fn has_top_level(&self, o: usize, c: usize, pred: impl Fn(&T) -> bool) -> bool {
        let mut depth = 0;
        for i in o + 1..c {
            let k = self.k(i);
            if is_open(k) {
                depth += 1;
            } else if is_close(k) {
                depth -= 1;
            } else if depth == 0 && pred(k) {
                return true;
            }
        }
        false
    }

    /// Whether the bracket pair `o`..`c` holds a comma-separated list.
    fn is_list(&self, o: usize, c: usize) -> bool {
        let prev = if o > 0 { Some(self.k(o - 1)) } else { None };
        match self.k(o) {
            T::LBracket => !self.has_top_level(o, c, |k| matches!(k, T::Semi { .. })),
            T::LParen => {
                if self.has_top_level(o, c, |k| matches!(k, T::Semi { .. })) {
                    return false;
                }
                // A call, parameter list, or tuple pattern; a bare
                // parenthesized expression only when it has commas already.
                self.top_level_commas(o, c) > 0
                    || matches!(
                        prev,
                        Some(
                            T::Ident(_) | T::RParen | T::RBracket | T::Gt | T::Keyword(K::SelfType)
                        )
                    )
            }
            T::LBrace => {
                if self.has_top_level(o, c, |k| matches!(k, T::FatArrow)) {
                    return true;
                }
                // The statement header: `struct S {`, `enum E {`, `import a::{`.
                let mut j = o;
                while j > 0 {
                    j -= 1;
                    match self.k(j) {
                        T::Semi { .. } | T::LBrace | T::RBrace => break,
                        T::Keyword(K::Struct | K::Enum | K::Import | K::Use) => {
                            // `impl Foo for S {` and `fn f() -> S {` are blocks.
                            return !(o..c).is_empty()
                                && !self.toks[j..o].iter().any(|t| {
                                    matches!(t.kind, T::Keyword(K::Fn | K::Impl | K::Trait))
                                });
                        }
                        T::Keyword(
                            K::Fn
                            | K::Impl
                            | K::Trait
                            | K::If
                            | K::While
                            | K::For
                            | K::Match
                            | K::Loop,
                        ) => break,
                        _ => {}
                    }
                }
                // A struct literal: `Name { field: ...` or `Name { ..base }`.
                let named = matches!(prev, Some(T::Ident(_) | T::Gt | T::Keyword(K::SelfType)));
                let first = o + 1;
                named
                    && first < c
                    && (matches!(self.k(first), T::DotDot)
                        || (matches!(self.k(first), T::Ident(_))
                            && first + 1 < c
                            && matches!(self.k(first + 1), T::Colon | T::Comma)
                            && !self.is_block_header(o)))
            }
            _ => false,
        }
    }

    /// Whether `{` at `o` follows a control-flow header (`if x {`), where it
    /// opens a block even if the block starts with `a: ...`.
    fn is_block_header(&self, o: usize) -> bool {
        let mut j = o;
        while j > 0 {
            j -= 1;
            match self.k(j) {
                T::Semi { .. } | T::LBrace | T::RBrace => return false,
                T::Keyword(K::If | K::While | K::For | K::Match | K::Loop | K::Else | K::Fn) => {
                    return true
                }
                _ => {}
            }
        }
        false
    }

    /// Splits tokens and comments into lines.
    fn build_lines(&self) -> Vec<Line> {
        let mut lines: Vec<Line> = vec![Line::default()];
        let push_break = |lines: &mut Vec<Line>, nl: usize| {
            if nl == 0 {
                return;
            }
            if !lines.last().unwrap().elems.is_empty() {
                lines.push(Line::default());
            }
            if nl >= 2 {
                lines.last_mut().unwrap().blank_before = true;
            }
        };
        let n = self.toks.len();
        let first = self.toks.first().map_or(self.src.len(), |t| t.span.start);
        let mut gaps = vec![scan_gap(&self.src[..first])];
        for i in 0..n {
            let a = self.toks[i].span.end;
            let b = self
                .toks
                .get(i + 1)
                .map_or(self.src.len(), |t| t.span.start);
            gaps.push(scan_gap(&self.src[a..b]));
        }
        for (gi, gap) in gaps.iter().enumerate() {
            for (nl, c) in &gap.items {
                // Newlines before the very first element don't make blank lines.
                let nl = if gi == 0 && lines.len() == 1 && lines[0].elems.is_empty() {
                    0
                } else {
                    *nl
                };
                push_break(&mut lines, nl);
                lines
                    .last_mut()
                    .unwrap()
                    .elems
                    .push(Elem::Comment(normalize_comment(c)));
            }
            if gi == n {
                break;
            }
            let nl = if gi == 0 && lines.len() == 1 && lines[0].elems.is_empty() {
                0
            } else {
                gap.trailing_newlines
            };
            push_break(&mut lines, nl);
            // A line comment always ends its line.
            if let Some(Elem::Comment(c)) = lines.last().unwrap().elems.last() {
                if c.starts_with("//") {
                    lines.push(Line::default());
                }
            }
            if !self.drop[gi] {
                lines.last_mut().unwrap().elems.push(Elem::Tok(gi));
            }
        }
        lines.retain(|l| !l.elems.is_empty());
        lines
    }

    /// The spacing between token `i` and token `j` (the next one printed);
    /// `None` keeps the source's choice.
    fn space(&self, i: usize, j: usize) -> Option<bool> {
        let p = self.k(i);
        let nx = self.k(j);
        use T::*;
        // Tokens that never have a space before them.
        if matches!(
            nx,
            RParen | RBracket | Comma | Semi { .. } | Question | Colon | Dot | ColonColon
        ) {
            if matches!(nx, Dot) && matches!(p, DotDot) {
                return None;
            }
            return Some(false);
        }
        // `chan<- T`: a send-only channel type.
        if matches!(nx, LArrow) && matches!(p, Keyword(K::Chan)) {
            return Some(false);
        }
        if matches!(p, LArrow) && i > 0 && matches!(self.k(i - 1), Keyword(K::Chan)) {
            return Some(true);
        }
        // Import groups: `std::io::{Read, Write}`.
        if matches!(p, LBrace) && i > 0 && matches!(self.k(i - 1), ColonColon) {
            return Some(false);
        }
        if matches!(nx, RBrace) && self.tight_close(j) {
            return Some(false);
        }
        // `-> !` / `) !` (never returns).
        if matches!(p, Bang)
            && i > 0
            && matches!(self.k(i - 1), Arrow | RParen)
            && self.space_in_source(i - 1)
        {
            return Some(true);
        }
        // A tuple or array type after a name or a parameter list:
        // `x (i32, i32)`, `fun f() [u8; 4]`.
        if matches!(p, RParen | Ident(_))
            && matches!(nx, LParen | LBracket)
            && self.space_in_source(i)
        {
            return Some(true);
        }
        if matches!(p, LParen | LBracket | Dot | ColonColon | Hash | Bang) {
            return Some(false);
        }
        // `@test`, `@derive(...)`: an attribute (at the start of its line).
        if matches!(p, At)
            && (i == 0 || self.newline_after(i - 1) || !matches!(self.k(i - 1), Ident(_)))
        {
            return Some(false);
        }
        if is_maybe_unary(p) && self.unary[i] {
            return Some(false);
        }
        if matches!(p, LBrace) {
            return Some(!matches!(nx, RBrace));
        }
        if matches!(nx, RBrace) {
            return Some(!matches!(p, LBrace));
        }
        if matches!(p, Comma | Semi { .. } | Colon) {
            return Some(true);
        }
        if is_binary_op(p) || is_binary_op(nx) {
            return Some(true);
        }
        // `<`, `>`, `<<`, `>>` are comparisons and shifts when the author put a
        // space before them; generic brackets never have one.
        if self.angle_binary(i) || self.angle_binary(j) {
            return Some(true);
        }
        if is_maybe_unary(p) {
            // Binary use.
            return Some(true);
        }
        if is_maybe_unary(nx) {
            return Some(if self.unary[j] {
                is_word(p) || matches!(p, RParen | RBracket) || self.space_in_source(i)
            } else {
                true
            });
        }
        match nx {
            LParen => {
                if matches!(p, Keyword(K::Mut)) && i > 0 && matches!(self.k(i - 1), Keyword(K::Fn))
                {
                    return Some(false);
                }
                return match p {
                    Ident(_)
                    | RParen
                    | RBracket
                    | Keyword(K::Fn | K::SelfValue | K::SelfType)
                    | Str(_)
                    | RawStr(_) => Some(false),
                    Keyword(_) => Some(true),
                    Gt | Lt | Pipe | OrOr | Shr => None,
                    _ => None,
                };
            }
            LBracket => {
                return match p {
                    Ident(_) | RParen | RBracket | Keyword(K::SelfValue) | Str(_) => Some(false),
                    Keyword(_) => Some(true),
                    _ => None,
                };
            }
            LBrace => {
                return match p {
                    Gt | Lt | Pipe | OrOr => None,
                    _ => Some(true),
                }
            }
            Bang => {
                return match p {
                    Ident(_) => Some(false),
                    Keyword(_) => Some(true),
                    _ => None,
                }
            }
            _ => {}
        }
        if (is_word(p) || matches!(p, RParen | RBracket | RBrace)) && is_word(nx) {
            return Some(true);
        }
        None
    }

    fn render(&self, lines: Vec<Line>) -> String {
        // Bracket stack entries: (token index of the opener, indent of its line).
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut out_lines: Vec<(usize, String, bool, bool)> = Vec::new(); // (depth, text, blank_before, top-level item start)
        let mut prev_last: Option<usize> = None; // last token of the previous code line
        let mut prev_first: Option<usize> = None; // first token of the previous code line
        let mut prev_was_close_item = false;
        for line in &lines {
            let first_tok = line.elems.iter().find_map(|e| match e {
                Elem::Tok(i) => Some(*i),
                _ => None,
            });
            let starts_with_tok = matches!(line.elems.first(), Some(Elem::Tok(_)));
            // Indentation.
            let mut indent;
            let leading_close = starts_with_tok && first_tok.is_some_and(|i| is_close(self.k(i)));
            if leading_close {
                let i = first_tok.unwrap();
                indent = stack
                    .iter()
                    .rev()
                    .find(|(o, _)| self.close_of[*o] == Some(i))
                    .map_or(0, |(_, ind)| *ind);
            } else {
                indent = stack.last().map_or(0, |(_, ind)| ind + 1);
                // A continued line.
                if let (Some(pl), Some(ft)) = (prev_last, first_tok) {
                    let starts_chain = starts_with_tok
                        && (matches!(self.k(ft), T::Dot | T::Question)
                            || (is_binary_op(self.k(ft)) && !matches!(self.k(ft), T::Eq)));
                    let pk = self.k(pl);
                    let open_ends = is_open(pk)
                        || matches!(pk, T::Comma | T::Semi { .. })
                        || pk.ends_statement()
                        || (matches!(pk, T::Bang)
                            && pl > 0
                            && matches!(self.k(pl - 1), T::Arrow | T::RParen))
                        || self.add_comma[pl];
                    let in_list_of_opener = stack.last().is_some_and(|(o, _)| *o == pl);
                    let after_attr = prev_first.is_some_and(|f| matches!(self.k(f), T::At));
                    if starts_chain
                        || (!open_ends
                            && !in_list_of_opener
                            && !matches!(pk, T::Hash)
                            && !after_attr)
                    {
                        indent += 1;
                    }
                }
            }
            // Text.
            let mut text = String::new();
            let mut last: Option<usize> = None;
            for e in &line.elems {
                match e {
                    Elem::Comment(c) => {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(c);
                    }
                    Elem::Tok(i) => {
                        if let Some(l) = last {
                            let sp = self
                                .space(l, *i)
                                .unwrap_or_else(|| self.space_in_source_between(l, *i));
                            if sp {
                                text.push(' ');
                            }
                        } else if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(self.text(*i));
                        if self.add_comma[*i] {
                            text.push(',');
                        }
                        // Track brackets.
                        let k = self.k(*i);
                        if is_open(k) {
                            stack.push((*i, indent_base(indent, leading_close)));
                        } else if is_close(k) {
                            if let Some(pos) = stack
                                .iter()
                                .rposition(|(o, _)| self.close_of[*o] == Some(*i))
                            {
                                stack.truncate(pos);
                            }
                        }
                        last = Some(*i);
                    }
                }
            }
            let depth0 = stack.is_empty() && !leading_close || (leading_close && indent == 0);
            let item_start = indent == 0 && starts_with_tok && !leading_close;
            let mut blank = line.blank_before;
            // One blank line between top-level items.
            if item_start
                && prev_was_close_item
                && !matches!(self.k(first_tok.unwrap()), T::Keyword(K::Else))
            {
                blank = true;
            }
            prev_was_close_item = indent == 0 && leading_close && depth0;
            if last.is_some() {
                prev_last = last;
                prev_first = first_tok;
            }
            out_lines.push((indent, text, blank, item_start));
        }
        // Blank lines: none at the start, just inside an opening bracket, or
        // just before a closing one.
        let mut out = String::new();
        let mut i = 0;
        let mut prev_text = String::new();
        let mut after_imports = false;
        while i < out_lines.len() {
            let (ind, text, blank, _) = &out_lines[i];
            let prev_opens =
                prev_text.ends_with('{') || prev_text.ends_with('(') || prev_text.ends_with('[');
            let closes = text.starts_with('}') || text.starts_with(')') || text.starts_with(']');
            if (*blank || after_imports) && i > 0 && !prev_opens && !closes {
                out.push('\n');
            }
            // Sort runs of top-level single-line imports.
            if *ind == 0 && is_import(text) {
                let mut j = i;
                let mut run = Vec::new();
                while j < out_lines.len()
                    && out_lines[j].0 == 0
                    && is_import(&out_lines[j].1)
                    && (j == i || !out_lines[j].2)
                {
                    run.push(out_lines[j].1.clone());
                    j += 1;
                }
                run.sort_by_key(|a| import_key(a));
                run.dedup();
                for r in &run {
                    out.push_str(r);
                    out.push('\n');
                }
                prev_text = run.last().cloned().unwrap_or_default();
                i = j;
                after_imports = true;
                continue;
            }
            for _ in 0..*ind {
                out.push('\t');
            }
            out.push_str(text);
            out.push('\n');
            after_imports = false;
            prev_text = text.clone();
            i += 1;
        }
        out
    }

    /// Whether `}` at `c` closes an import group (`::{`).
    fn tight_close(&self, c: usize) -> bool {
        (0..c)
            .rev()
            .take(4096)
            .any(|o| self.close_of[o] == Some(c) && o > 0 && matches!(self.k(o - 1), T::ColonColon))
    }

    fn angle_binary(&self, i: usize) -> bool {
        matches!(self.k(i), T::Lt | T::Gt | T::Shl | T::Shr) && i > 0 && self.space_in_source(i - 1)
    }

    fn space_in_source_between(&self, i: usize, j: usize) -> bool {
        self.toks[j].span.start > self.toks[i].span.end
    }
}

fn indent_base(indent: usize, _leading_close: bool) -> usize {
    indent
}

fn is_import(text: &str) -> bool {
    text.starts_with("import ")
        && !text.contains("//")
        && !text.ends_with('{')
        && !text.contains("/*")
}

/// Sort key: `std` imports first, then the rest, alphabetically.
fn import_key(line: &str) -> (bool, String) {
    let path = line.trim_start_matches("import ").to_string();
    (!path.starts_with("std"), path.replace('{', "\0"))
}

/// The tokens that must be unchanged by formatting (commas and semicolons
/// may be added or removed) and the comments, whitespace-normalized.
fn fingerprint(src: &str) -> (Vec<String>, Vec<String>) {
    let lexed = kiln_lexer::lex(src);
    // Import statements may be reordered and merged: compare them as a set.
    let mut toks = Vec::new();
    let mut imports = std::collections::BTreeSet::new();
    let mut i = 0;
    let all = &lexed.tokens;
    while i < all.len() {
        let t = &all[i];
        let stmt_start = i == 0 || matches!(all[i - 1].kind, T::Semi { .. });
        if stmt_start && matches!(t.kind, T::Keyword(K::Import)) {
            let mut s = String::new();
            while i < all.len() && !matches!(all[i].kind, T::Semi { .. } | T::Eof) {
                if !matches!(all[i].kind, T::Comma) {
                    s.push_str(&src[all[i].span.start..all[i].span.end]);
                    s.push(' ');
                }
                i += 1;
            }
            imports.insert(s);
            continue;
        }
        if !matches!(t.kind, T::Semi { .. } | T::Comma | T::Eof) {
            toks.push(src[t.span.start..t.span.end].to_string());
        }
        i += 1;
    }
    toks.extend(imports);
    let mut comments = Vec::new();
    let mut prev = 0;
    for t in &lexed.tokens {
        for (_, c) in scan_gap(&src[prev..t.span.start]).items {
            comments.push(normalize_comment(&c));
        }
        prev = t.span.end.max(prev);
    }
    (toks, comments)
}

fn verify(src: &str, out: &str) -> Result<(), Error> {
    let (a, ca) = fingerprint(src);
    let (b, cb) = fingerprint(out);
    if a != b {
        let at = a
            .iter()
            .zip(&b)
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        return Err(Error::Internal(format!(
            "tokens changed near token {at}: {:?} became {:?}",
            a.get(at),
            b.get(at)
        )));
    }
    if ca != cb {
        return Err(Error::Internal("comments changed".into()));
    }
    let reparsed = kiln_parser::parse(out);
    if let Some(e) = reparsed.errors.first() {
        return Err(Error::Internal(format!(
            "output does not parse: {}",
            e.message
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
