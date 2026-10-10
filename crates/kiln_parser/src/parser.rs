//! Recursive-descent parser with Pratt-style expression parsing.

use crate::ast::*;
use kiln_lexer::{lex_at, Diagnostic, Keyword as Kw, StrPart, Token, TokenKind as T};

/// Error marker: the diagnostic has already been recorded.
#[derive(Debug)]
pub(crate) struct Reported;

type PResult<R> = Result<R, Reported>;

/// The parser state.
pub(crate) struct Parser<'s> {
    src: &'s str,
    tokens: Vec<Token>,
    pos: usize,
    docs: Vec<(Span, String)>,
    doc_idx: usize,
    pub(crate) errors: Vec<Diagnostic>,
    /// When true, `Path {` is not parsed as a struct literal (conditions, scrutinees).
    no_struct: bool,
}

/// Binding power of binary operators (higher binds tighter).
fn binop(kind: &T) -> Option<(BinOp, u8)> {
    Some(match kind {
        T::OrOr => (BinOp::Or, 2),
        T::AndAnd => (BinOp::And, 3),
        T::EqEq => (BinOp::Eq, 4),
        T::Ne => (BinOp::Ne, 4),
        T::Lt => (BinOp::Lt, 4),
        T::Le => (BinOp::Le, 4),
        T::Gt => (BinOp::Gt, 4),
        T::Ge => (BinOp::Ge, 4),
        T::Pipe => (BinOp::BitOr, 5),
        T::Caret => (BinOp::BitXor, 6),
        T::Amp => (BinOp::BitAnd, 7),
        T::Shl => (BinOp::Shl, 8),
        T::Shr => (BinOp::Shr, 8),
        T::Plus => (BinOp::Add, 9),
        T::Minus => (BinOp::Sub, 9),
        T::Star => (BinOp::Mul, 10),
        T::Slash => (BinOp::Div, 10),
        T::Percent => (BinOp::Rem, 10),
        _ => return None,
    })
}

/// Binding power of `as` casts.
const CAST_PREC: u8 = 11;

fn assign_op(kind: &T) -> Option<Option<BinOp>> {
    Some(match kind {
        T::Eq => None,
        T::PlusEq => Some(BinOp::Add),
        T::MinusEq => Some(BinOp::Sub),
        T::StarEq => Some(BinOp::Mul),
        T::SlashEq => Some(BinOp::Div),
        T::PercentEq => Some(BinOp::Rem),
        T::CaretEq => Some(BinOp::BitXor),
        T::AmpEq => Some(BinOp::BitAnd),
        T::PipeEq => Some(BinOp::BitOr),
        T::ShlEq => Some(BinOp::Shl),
        T::ShrEq => Some(BinOp::Shr),
        _ => return None,
    })
}

impl<'s> Parser<'s> {
    /// Creates a parser over lexed tokens. `src` is the full file text the spans index into.
    pub(crate) fn new(src: &'s str, tokens: Vec<Token>, docs: Vec<(Span, String)>) -> Self {
        Parser {
            src,
            tokens,
            pos: 0,
            docs,
            doc_idx: 0,
            errors: Vec::new(),
            no_struct: false,
        }
    }

    // ----- token helpers -------------------------------------------------

    fn peek(&self) -> &T {
        &self.tokens[self.pos].kind
    }

    fn nth(&self, n: usize) -> &T {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            Span::default()
        } else {
            self.tokens[self.pos - 1].span
        }
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, k: &T) -> bool {
        self.peek() == k
    }

    fn at_kw(&self, k: Kw) -> bool {
        *self.peek() == T::Keyword(k)
    }

    fn at_semi(&self) -> bool {
        matches!(self.peek(), T::Semi { .. })
    }

    fn at_eof(&self) -> bool {
        *self.peek() == T::Eof
    }

    fn eat(&mut self, k: &T) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        self.eat(&T::Keyword(k))
    }

    fn skip_semis(&mut self) {
        while self.at_semi() {
            self.bump();
        }
    }

    fn skip_implicit_semis(&mut self) {
        while matches!(self.peek(), T::Semi { implicit: true }) {
            self.bump();
        }
    }

    fn error_at<R>(&mut self, msg: impl Into<String>, span: Span) -> PResult<R> {
        self.errors.push(Diagnostic::error(msg, span));
        Err(Reported)
    }

    fn unexpected<R>(&mut self, expected: &str) -> PResult<R> {
        let found = self.peek().describe();
        let span = self.span();
        self.error_at(format!("expected {expected}, found {found}"), span)
    }

    fn expect(&mut self, k: &T) -> PResult<Span> {
        if self.at(k) {
            Ok(self.bump().span)
        } else {
            let what = format!("`{}`", k.punct_str().unwrap_or("?"));
            self.unexpected(&what)
        }
    }

    fn expect_kw(&mut self, k: Kw) -> PResult<Span> {
        if self.at_kw(k) {
            Ok(self.bump().span)
        } else {
            self.unexpected(&format!("`{}`", k.as_str()))
        }
    }

    fn expect_ident(&mut self) -> PResult<Ident> {
        match self.peek().clone() {
            T::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            T::Keyword(k) if k.is_reserved() => {
                let span = self.span();
                self.error_at(format!("`{}` is a reserved keyword", k.as_str()), span)
            }
            _ => self.unexpected("identifier"),
        }
    }

    fn expect_lifetime(&mut self) -> PResult<Ident> {
        match self.peek().clone() {
            T::Lifetime(name) => Ok(Ident {
                name,
                span: self.bump().span,
            }),
            _ => self.unexpected("lifetime"),
        }
    }

    /// Expects `>`, splitting `>>`, `>=`, `>>=` as needed for nested generics.
    fn expect_gt(&mut self) -> PResult<()> {
        let rest = match self.peek() {
            T::Gt => {
                self.bump();
                return Ok(());
            }
            T::Shr => T::Gt,
            T::Ge => T::Eq,
            T::ShrEq => T::Ge,
            _ => return self.unexpected("`>`"),
        };
        let tok = &mut self.tokens[self.pos];
        tok.kind = rest;
        tok.span.start += 1;
        Ok(())
    }

    fn at_gt(&self) -> bool {
        matches!(self.peek(), T::Gt | T::Shr | T::Ge | T::ShrEq)
    }

    fn with_no_struct<R>(&mut self, v: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = std::mem::replace(&mut self.no_struct, v);
        let r = f(self);
        self.no_struct = old;
        r
    }

    /// Parses `item (, item)* [,]` up to and including `close`. Newlines between
    /// elements are ignored, so multi-line lists parse with or without a trailing comma.
    fn comma_list<R>(
        &mut self,
        close: &T,
        mut f: impl FnMut(&mut Self) -> PResult<R>,
    ) -> PResult<Vec<R>> {
        let mut out = Vec::new();
        loop {
            self.skip_implicit_semis();
            if self.eat(close) {
                return Ok(out);
            }
            out.push(f(self)?);
            self.skip_implicit_semis();
            if self.eat(&T::Comma) {
                continue;
            }
            if self.eat(close) {
                return Ok(out);
            }
            let what = format!("`,` or `{}`", close.punct_str().unwrap_or("?"));
            return self.unexpected(&what);
        }
    }

    /// Parses `{ elem sep elem ... }` where `,`, `;`, or a newline separates
    /// elements (fields, variants, arms).
    fn brace_list<R>(&mut self, mut f: impl FnMut(&mut Self) -> PResult<R>) -> PResult<Vec<R>> {
        self.expect(&T::LBrace)?;
        let mut out = Vec::new();
        loop {
            self.skip_semis();
            if self.eat(&T::RBrace) {
                return Ok(out);
            }
            if self.at_eof() {
                return self.unexpected("`}`");
            }
            out.push(f(self)?);
            if self.eat(&T::Comma) || self.at_semi() || self.at(&T::RBrace) {
                continue;
            }
            return self.unexpected("`,`, newline, or `}`");
        }
    }

    // ----- recovery -------------------------------------------------------

    fn at_item_start(&self) -> bool {
        matches!(
            self.peek(),
            T::Keyword(
                Kw::Fn
                    | Kw::Struct
                    | Kw::Enum
                    | Kw::Trait
                    | Kw::Impl
                    | Kw::Import
                    | Kw::Const
                    | Kw::Static
                    | Kw::Type
                    | Kw::Mod
                    | Kw::Pub
                    | Kw::Use
            ) | T::Hash
        ) || (self.at_kw(Kw::Unsafe) && *self.nth(1) == T::Keyword(Kw::Fn))
    }

    /// Skips tokens until the start of the next item at nesting depth 0.
    fn recover_item(&mut self) {
        let mut depth = 0usize;
        let start = self.pos;
        loop {
            match self.peek() {
                T::Eof => return,
                T::LBrace | T::LParen | T::LBracket => depth += 1,
                T::RBrace | T::RParen | T::RBracket => depth = depth.saturating_sub(1),
                _ if depth == 0 && self.pos > start && self.at_item_start() => return,
                _ => {}
            }
            self.bump();
        }
    }

    /// Skips to the end of the current statement: a `;`/newline at this
    /// nesting level, or the enclosing `}`.
    fn recover_stmt(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                T::Eof => return,
                T::Semi { .. } if depth == 0 => {
                    self.bump();
                    return;
                }
                T::RBrace if depth == 0 => return,
                T::LBrace | T::LParen | T::LBracket => depth += 1,
                T::RBrace | T::RParen | T::RBracket => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
    }

    // ----- items ----------------------------------------------------------

    /// Parses a whole file.
    pub(crate) fn parse_file(&mut self) -> File {
        File {
            items: self.parse_items(true),
        }
    }

    fn parse_items(&mut self, top: bool) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.skip_semis();
            if self.at_eof() || (!top && self.at(&T::RBrace)) {
                return items;
            }
            let before = self.pos;
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(Reported) => {
                    self.recover_item();
                    if self.pos == before {
                        self.bump();
                    }
                }
            }
        }
    }

    /// Collects `///` doc comments located between `boundary` and `pos`.
    fn take_docs(&mut self, pos: usize, boundary: usize) -> Option<String> {
        let mut lines = Vec::new();
        while let Some((span, text)) = self.docs.get(self.doc_idx) {
            if span.start >= pos {
                break;
            }
            if span.start >= boundary {
                lines.push(text.clone());
            }
            self.doc_idx += 1;
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn parse_attr(&mut self) -> PResult<Attribute> {
        let start = self.expect(&T::Hash)?;
        self.expect(&T::LBracket)?;
        let name = self.expect_ident()?;
        let mut args = None;
        if self.at(&T::LParen) {
            let open = self.bump().span;
            let mut depth = 1;
            loop {
                match self.peek() {
                    T::Eof => return self.unexpected("`)`"),
                    T::LParen => depth += 1,
                    T::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                self.bump();
            }
            let close = self.bump().span;
            args = Some(self.src[open.end..close.start].trim().to_string());
        }
        let end = self.expect(&T::RBracket)?;
        Ok(Attribute {
            name,
            args,
            span: start.to(end),
        })
    }

    fn parse_vis(&mut self) -> Visibility {
        if !self.eat_kw(Kw::Pub) {
            return Visibility::Private;
        }
        if self.at(&T::LParen)
            && *self.nth(1) == T::Ident("pkg".into())
            && *self.nth(2) == T::RParen
        {
            for _ in 0..3 {
                self.bump();
            }
            return Visibility::Package;
        }
        Visibility::Public
    }

    fn opt_eq_expr(&mut self) -> PResult<Option<Expr>> {
        if self.eat(&T::Eq) {
            Ok(Some(self.parse_expr()?))
        } else {
            Ok(None)
        }
    }

    fn parse_item(&mut self) -> PResult<Item> {
        let start = self.span();
        let boundary = if self.pos == 0 {
            0
        } else {
            self.prev_span().end
        };
        let doc = self.take_docs(start.start, boundary);
        let mut attrs = Vec::new();
        while self.at(&T::Hash) {
            attrs.push(self.parse_attr()?);
            self.skip_semis();
        }
        let vis = self.parse_vis();
        let kind = match self.peek() {
            T::Keyword(Kw::Fn) => ItemKind::Fn(self.parse_fn(false)?),
            T::Keyword(Kw::Unsafe) if *self.nth(1) == T::Keyword(Kw::Fn) => {
                self.bump();
                ItemKind::Fn(self.parse_fn(true)?)
            }
            T::Keyword(Kw::Struct) => ItemKind::Struct(self.parse_struct()?),
            T::Keyword(Kw::Enum) => ItemKind::Enum(self.parse_enum()?),
            T::Keyword(Kw::Trait) => ItemKind::Trait(self.parse_trait()?),
            T::Keyword(Kw::Impl) => ItemKind::Impl(self.parse_impl()?),
            T::Keyword(Kw::Import) => {
                self.bump();
                ItemKind::Import(self.parse_import_tree()?)
            }
            T::Keyword(Kw::Use) => {
                let span = self.span();
                self.errors.push(
                    Diagnostic::error("`use` is reserved; Kiln imports with `import`", span)
                        .with_help("write `import path::to::item`"),
                );
                self.bump();
                ItemKind::Import(self.parse_import_tree()?)
            }
            T::Keyword(Kw::Const) => {
                self.bump();
                let name = self.expect_ident()?;
                self.expect(&T::Colon)?;
                let ty = self.parse_type()?;
                let value = self.opt_eq_expr()?;
                ItemKind::Const { name, ty, value }
            }
            T::Keyword(Kw::Static) => {
                self.bump();
                let mutable = self.eat_kw(Kw::Mut);
                let name = self.expect_ident()?;
                self.expect(&T::Colon)?;
                let ty = self.parse_type()?;
                self.expect(&T::Eq)?;
                let value = self.parse_expr()?;
                ItemKind::Static {
                    name,
                    mutable,
                    ty,
                    value,
                }
            }
            T::Keyword(Kw::Type) => {
                self.bump();
                let name = self.expect_ident()?;
                let generics = self.parse_generics()?;
                let ty = if self.eat(&T::Eq) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                ItemKind::TypeAlias { name, generics, ty }
            }
            T::Keyword(Kw::Mod) => {
                self.bump();
                let name = self.expect_ident()?;
                let items = if self.eat(&T::LBrace) {
                    let items = self.parse_items(false);
                    self.expect(&T::RBrace)?;
                    Some(items)
                } else {
                    None
                };
                ItemKind::Mod { name, items }
            }
            _ => {
                return self
                    .unexpected("item (`fn`, `struct`, `enum`, `trait`, `impl`, `import`, ...)")
            }
        };
        Ok(Item {
            kind,
            vis,
            attrs,
            doc,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_fn(&mut self, is_unsafe: bool) -> PResult<FnDecl> {
        self.expect_kw(Kw::Fn)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.expect(&T::LParen)?;
        self.skip_implicit_semis();
        let self_param = self.parse_self_param()?;
        let params = match self_param {
            Some(_) if self.eat(&T::RParen) => Vec::new(),
            Some(_) => {
                self.expect(&T::Comma)?;
                self.comma_list(&T::RParen, Self::parse_param)?
            }
            None => self.comma_list(&T::RParen, Self::parse_param)?,
        };
        let ret = if self.eat(&T::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        generics.where_clause = self.parse_where()?;
        let body = if self.at(&T::LBrace) {
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(FnDecl {
            name,
            is_unsafe,
            generics,
            self_param,
            params,
            ret,
            body,
        })
    }

    /// Parses `self`, `mut self`, `&self`, `&mut self`, `&'a self`, `&'a mut self`.
    fn parse_self_param(&mut self) -> PResult<Option<SelfParam>> {
        let is_self = |t: &T| *t == T::Keyword(Kw::SelfValue);
        let is_mut = |t: &T| *t == T::Keyword(Kw::Mut);
        if is_self(self.peek()) {
            self.bump();
            return Ok(Some(SelfParam::Value { mutable: false }));
        }
        if is_mut(self.peek()) && is_self(self.nth(1)) {
            self.bump();
            self.bump();
            return Ok(Some(SelfParam::Value { mutable: true }));
        }
        if !self.at(&T::Amp) {
            return Ok(None);
        }
        let mut i = 1;
        let lifetime = if let T::Lifetime(name) = self.nth(1).clone() {
            i += 1;
            Some(Ident {
                name,
                span: self.tokens[self.pos + 1].span,
            })
        } else {
            None
        };
        let mutable = is_mut(self.nth(i));
        if mutable {
            i += 1;
        }
        if !is_self(self.nth(i)) {
            return Ok(None);
        }
        for _ in 0..=i {
            self.bump();
        }
        Ok(Some(SelfParam::Ref { lifetime, mutable }))
    }

    fn parse_param(&mut self) -> PResult<Param> {
        let pat = self.parse_pattern_no_or()?;
        self.expect(&T::Colon)?;
        let ty = self.parse_type()?;
        Ok(Param {
            span: pat.span.to(ty.span),
            pat,
            ty,
        })
    }

    fn parse_generics(&mut self) -> PResult<Generics> {
        let mut g = Generics::default();
        if !self.eat(&T::Lt) {
            return Ok(g);
        }
        loop {
            if self.at_gt() {
                self.expect_gt()?;
                return Ok(g);
            }
            let param = match self.peek().clone() {
                T::Lifetime(_) => {
                    let name = self.expect_lifetime()?;
                    let mut bounds = Vec::new();
                    if self.eat(&T::Colon) {
                        loop {
                            bounds.push(self.expect_lifetime()?);
                            if !self.eat(&T::Plus) {
                                break;
                            }
                        }
                    }
                    GenericParam::Lifetime { name, bounds }
                }
                T::Keyword(Kw::Const) => {
                    self.bump();
                    let name = self.expect_ident()?;
                    self.expect(&T::Colon)?;
                    let ty = self.parse_type()?;
                    GenericParam::Const { name, ty }
                }
                _ => {
                    let name = self.expect_ident()?;
                    let bounds = if self.eat(&T::Colon) {
                        self.parse_bounds()?
                    } else {
                        Vec::new()
                    };
                    let default = if self.eat(&T::Eq) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    GenericParam::Type {
                        name,
                        bounds,
                        default,
                    }
                }
            };
            g.params.push(param);
            if !self.eat(&T::Comma) {
                self.expect_gt()?;
                return Ok(g);
            }
        }
    }

    fn parse_bounds(&mut self) -> PResult<Vec<Bound>> {
        let mut out = Vec::new();
        loop {
            let b = match self.peek() {
                T::Lifetime(_) => Bound::Lifetime(self.expect_lifetime()?),
                T::Question => {
                    self.bump();
                    Bound::Trait {
                        path: self.parse_type_path()?,
                        maybe: true,
                    }
                }
                _ => Bound::Trait {
                    path: self.parse_type_path()?,
                    maybe: false,
                },
            };
            out.push(b);
            if !self.eat(&T::Plus) {
                return Ok(out);
            }
        }
    }

    fn parse_where(&mut self) -> PResult<Vec<WherePredicate>> {
        let mut preds = Vec::new();
        if !self.eat_kw(Kw::Where) {
            return Ok(preds);
        }
        loop {
            self.skip_implicit_semis();
            let ty = self.parse_type()?;
            self.expect(&T::Colon)?;
            let bounds = self.parse_bounds()?;
            preds.push(WherePredicate {
                span: ty.span.to(self.prev_span()),
                ty,
                bounds,
            });
            if !self.eat(&T::Comma) {
                return Ok(preds);
            }
            self.skip_implicit_semis();
            if self.at(&T::LBrace) {
                return Ok(preds);
            }
        }
    }

    fn parse_struct(&mut self) -> PResult<StructDecl> {
        self.expect_kw(Kw::Struct)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        let fields = if self.at(&T::LParen) {
            let f = self.parse_tuple_fields()?;
            generics.where_clause = self.parse_where()?;
            f
        } else {
            generics.where_clause = self.parse_where()?;
            if self.at(&T::LBrace) {
                Fields::Named(self.brace_list(Self::parse_field_decl)?)
            } else {
                Fields::Unit
            }
        };
        Ok(StructDecl {
            name,
            generics,
            fields,
        })
    }

    fn parse_tuple_fields(&mut self) -> PResult<Fields> {
        self.expect(&T::LParen)?;
        let tys = self.comma_list(&T::RParen, |p| {
            p.parse_vis();
            p.parse_type()
        })?;
        Ok(Fields::Tuple(tys))
    }

    fn parse_field_decl(&mut self) -> PResult<FieldDecl> {
        let start = self.span();
        let vis = self.parse_vis();
        let name = self.expect_ident()?;
        self.expect(&T::Colon)?;
        let ty = self.parse_type()?;
        Ok(FieldDecl {
            vis,
            name,
            span: start.to(ty.span),
            ty,
        })
    }

    fn parse_enum(&mut self) -> PResult<EnumDecl> {
        self.expect_kw(Kw::Enum)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        generics.where_clause = self.parse_where()?;
        let variants = self.brace_list(|p| {
            let name = p.expect_ident()?;
            let fields = match p.peek() {
                T::LParen => p.parse_tuple_fields()?,
                T::LBrace => Fields::Named(p.brace_list(Self::parse_field_decl)?),
                _ => Fields::Unit,
            };
            let discriminant = p.opt_eq_expr()?;
            Ok(Variant {
                span: name.span.to(p.prev_span()),
                name,
                fields,
                discriminant,
            })
        })?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
        })
    }

    fn parse_assoc_items(&mut self) -> PResult<Vec<Item>> {
        self.expect(&T::LBrace)?;
        let items = self.parse_items(false);
        self.expect(&T::RBrace)?;
        Ok(items)
    }

    fn parse_trait(&mut self) -> PResult<TraitDecl> {
        self.expect_kw(Kw::Trait)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        let supertraits = if self.eat(&T::Colon) {
            self.parse_bounds()?
        } else {
            Vec::new()
        };
        generics.where_clause = self.parse_where()?;
        let items = self.parse_assoc_items()?;
        Ok(TraitDecl {
            name,
            generics,
            supertraits,
            items,
        })
    }

    fn parse_impl(&mut self) -> PResult<ImplDecl> {
        self.expect_kw(Kw::Impl)?;
        let mut generics = self.parse_generics()?;
        let first = self.parse_type()?;
        let (trait_ref, self_ty) = if self.eat_kw(Kw::For) {
            let path = match first.kind {
                TypeKind::Path(p) => p,
                _ => return self.error_at("expected a trait path before `for`", first.span),
            };
            (Some(path), self.parse_type()?)
        } else {
            (None, first)
        };
        generics.where_clause = self.parse_where()?;
        let items = self.parse_assoc_items()?;
        Ok(ImplDecl {
            generics,
            trait_ref,
            self_ty,
            items,
        })
    }

    /// An identifier usable as a path segment (`self`, `Self`, `chan` allowed).
    fn path_ident(&mut self) -> PResult<Ident> {
        let name = match self.peek() {
            T::Ident(s) => s.clone(),
            T::Keyword(k @ (Kw::SelfValue | Kw::SelfType | Kw::Chan)) => k.as_str().to_string(),
            _ => return self.unexpected("identifier"),
        };
        Ok(Ident {
            name,
            span: self.bump().span,
        })
    }

    fn parse_import_tree(&mut self) -> PResult<ImportTree> {
        let mut prefix = Vec::new();
        loop {
            if !prefix.is_empty() && self.eat(&T::Star) {
                return Ok(ImportTree::Glob { prefix });
            }
            if !prefix.is_empty() && self.eat(&T::LBrace) {
                let items = self.comma_list(&T::RBrace, Self::parse_import_tree)?;
                return Ok(ImportTree::Group { prefix, items });
            }
            prefix.push(self.path_ident()?);
            if !self.eat(&T::ColonColon) {
                break;
            }
        }
        let alias = if self.eat_kw(Kw::As) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(ImportTree::Path {
            path: prefix,
            alias,
        })
    }

    // ----- types ----------------------------------------------------------

    fn parse_generic_args(&mut self) -> PResult<Vec<GenericArg>> {
        self.expect(&T::Lt)?;
        let mut args = Vec::new();
        loop {
            if self.at_gt() {
                self.expect_gt()?;
                return Ok(args);
            }
            let arg = match self.peek().clone() {
                T::Lifetime(_) => GenericArg::Lifetime(self.expect_lifetime()?),
                T::Ident(_) if *self.nth(1) == T::Eq => {
                    let name = self.expect_ident()?;
                    self.bump();
                    GenericArg::Binding {
                        name,
                        ty: self.parse_type()?,
                    }
                }
                _ => GenericArg::Type(self.parse_type()?),
            };
            args.push(arg);
            if !self.eat(&T::Comma) {
                self.expect_gt()?;
                return Ok(args);
            }
        }
    }

    /// Parses a path in type position: `a::b::C<T>`.
    fn parse_type_path(&mut self) -> PResult<Path> {
        let start = self.span();
        let mut segments = Vec::new();
        loop {
            let ident = self.path_ident()?;
            let mut args = Vec::new();
            if self.at(&T::Lt) {
                args = self.parse_generic_args()?;
            } else if self.at(&T::ColonColon) && *self.nth(1) == T::Lt {
                self.bump();
                args = self.parse_generic_args()?;
            }
            segments.push(PathSegment { ident, args });
            if !(self.at(&T::ColonColon) && matches!(self.nth(1), T::Ident(_) | T::Keyword(_))) {
                break;
            }
            self.bump();
        }
        Ok(Path {
            segments,
            span: start.to(self.prev_span()),
        })
    }

    /// Parses a type.
    pub(crate) fn parse_type(&mut self) -> PResult<Type> {
        let start = self.span();
        let kind = match self.peek().clone() {
            T::Amp | T::AndAnd => {
                let double = self.bump().kind == T::AndAnd;
                let lifetime = match self.peek() {
                    T::Lifetime(_) => Some(self.expect_lifetime()?),
                    _ => None,
                };
                let mutable = self.eat_kw(Kw::Mut);
                let inner = Box::new(self.parse_type()?);
                let r = TypeKind::Ref {
                    lifetime,
                    mutable,
                    inner,
                };
                if double {
                    TypeKind::Ref {
                        lifetime: None,
                        mutable: false,
                        inner: Box::new(Type {
                            kind: r,
                            span: start.to(self.prev_span()),
                        }),
                    }
                } else {
                    r
                }
            }
            T::Star => {
                self.bump();
                let mutable = if self.eat_kw(Kw::Mut) {
                    true
                } else {
                    self.expect_kw(Kw::Const)?;
                    false
                };
                TypeKind::Ptr {
                    mutable,
                    inner: Box::new(self.parse_type()?),
                }
            }
            T::LBracket => {
                self.bump();
                let elem = Box::new(self.parse_type()?);
                if self.eat(&T::Semi { implicit: false }) {
                    let len = self.with_no_struct(false, Self::parse_expr)?;
                    self.expect(&T::RBracket)?;
                    TypeKind::Array(elem, Box::new(len))
                } else {
                    self.expect(&T::RBracket)?;
                    TypeKind::Slice(elem)
                }
            }
            T::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing = false;
                loop {
                    if self.eat(&T::RParen) {
                        break;
                    }
                    elems.push(self.parse_type()?);
                    trailing = self.eat(&T::Comma);
                    if !trailing {
                        self.expect(&T::RParen)?;
                        break;
                    }
                }
                if elems.len() == 1 && !trailing {
                    let inner = elems.pop().unwrap();
                    return Ok(Type {
                        kind: inner.kind,
                        span: start.to(self.prev_span()),
                    });
                }
                TypeKind::Tuple(elems)
            }
            T::Keyword(Kw::Fn) => {
                self.bump();
                self.expect(&T::LParen)?;
                let params = self.comma_list(&T::RParen, Self::parse_type)?;
                let ret = if self.eat(&T::Arrow) {
                    Some(Box::new(self.parse_type()?))
                } else {
                    None
                };
                TypeKind::Fn { params, ret }
            }
            T::Keyword(Kw::Dyn) => {
                self.bump();
                TypeKind::Dyn(self.parse_bounds()?)
            }
            T::Keyword(Kw::Impl) => {
                self.bump();
                TypeKind::Impl(self.parse_bounds()?)
            }
            T::Keyword(Kw::Chan) if *self.nth(1) == T::LArrow => {
                self.bump();
                self.bump();
                TypeKind::ChanSend(Box::new(self.parse_type()?))
            }
            T::Keyword(Kw::Chan) => {
                let span = self.span();
                self.errors.push(
                    Diagnostic::error("bidirectional `chan T` is not a type in Kiln", span)
                        .with_help("use `chan<- T` (send end) or `<-chan T` (receive end)"),
                );
                return Err(Reported);
            }
            T::LArrow => {
                self.bump();
                self.expect_kw(Kw::Chan)?;
                TypeKind::ChanRecv(Box::new(self.parse_type()?))
            }
            T::Bang => {
                self.bump();
                TypeKind::Never
            }
            T::Underscore => {
                self.bump();
                TypeKind::Infer
            }
            T::Ident(_) | T::Keyword(Kw::SelfType | Kw::SelfValue) => {
                TypeKind::Path(self.parse_type_path()?)
            }
            _ => return self.unexpected("type"),
        };
        Ok(Type {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    // ----- patterns -------------------------------------------------------

    /// Parses a pattern, including `a | b` alternatives.
    pub(crate) fn parse_pattern(&mut self) -> PResult<Pattern> {
        self.eat(&T::Pipe);
        let first = self.parse_pattern_no_or()?;
        if !self.at(&T::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&T::Pipe) {
            alts.push(self.parse_pattern_no_or()?);
        }
        let span = alts[0].span.to(alts.last().unwrap().span);
        Ok(Pattern {
            kind: PatternKind::Or(alts),
            span,
        })
    }

    /// Parses an (optionally negative) literal in pattern position, if present.
    fn lit_pattern(&mut self) -> PResult<Option<Lit>> {
        let negative = self.at(&T::Minus);
        let off = usize::from(negative);
        let lit = match self.nth(off).clone() {
            T::Int { value, suffix } => Lit::Int {
                value,
                suffix,
                negative,
            },
            T::Float { value, suffix, .. } => Lit::Float {
                value: if negative { -value } else { value },
                suffix,
            },
            _ if negative => return self.unexpected("number after `-` in pattern"),
            T::Str(parts) => match parts.as_slice() {
                [StrPart::Text(s)] => Lit::Str(s.clone()),
                _ => {
                    let span = self.span();
                    return self.error_at("interpolated strings are not allowed in patterns", span);
                }
            },
            T::RawStr(s) => Lit::Str(s),
            T::Char(c) => Lit::Char(c),
            T::Keyword(Kw::True) => Lit::Bool(true),
            T::Keyword(Kw::False) => Lit::Bool(false),
            _ => return Ok(None),
        };
        for _ in 0..=off {
            self.bump();
        }
        Ok(Some(lit))
    }

    fn parse_pattern_no_or(&mut self) -> PResult<Pattern> {
        let start = self.span();
        let kind = match self.peek().clone() {
            T::Underscore => {
                self.bump();
                PatternKind::Wild
            }
            T::DotDot => {
                self.bump();
                PatternKind::Rest
            }
            T::Amp | T::AndAnd => {
                let double = self.bump().kind == T::AndAnd;
                let mutable = self.eat_kw(Kw::Mut);
                let inner = Box::new(self.parse_pattern_no_or()?);
                let p = PatternKind::Ref { mutable, inner };
                if double {
                    PatternKind::Ref {
                        mutable: false,
                        inner: Box::new(Pattern {
                            kind: p,
                            span: start.to(self.prev_span()),
                        }),
                    }
                } else {
                    p
                }
            }
            T::Keyword(Kw::Mut) => {
                self.bump();
                PatternKind::Ident {
                    name: self.expect_ident()?,
                    mutable: true,
                    sub: None,
                }
            }
            T::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing = false;
                loop {
                    self.skip_implicit_semis();
                    if self.eat(&T::RParen) {
                        break;
                    }
                    elems.push(self.parse_pattern()?);
                    trailing = self.eat(&T::Comma);
                    if !trailing {
                        self.skip_implicit_semis();
                        self.expect(&T::RParen)?;
                        break;
                    }
                }
                if elems.len() == 1 && !trailing {
                    let mut p = elems.pop().unwrap();
                    p.span = start.to(self.prev_span());
                    return Ok(p);
                }
                PatternKind::Tuple(elems)
            }
            T::LBracket => {
                self.bump();
                PatternKind::Slice(self.comma_list(&T::RBracket, Self::parse_pattern)?)
            }
            T::Ident(_) | T::Keyword(Kw::SelfValue | Kw::SelfType) => {
                let path = self.parse_expr_path()?;
                match self.peek() {
                    T::LParen => {
                        self.bump();
                        let elems = self.comma_list(&T::RParen, Self::parse_pattern)?;
                        PatternKind::TupleStruct(path, elems)
                    }
                    T::LBrace => self.parse_struct_pattern(path)?,
                    _ if path.segments.len() == 1 && path.segments[0].args.is_empty() => {
                        let name = path.segments.into_iter().next().unwrap().ident;
                        let sub = if self.eat(&T::At) {
                            Some(Box::new(self.parse_pattern_no_or()?))
                        } else {
                            None
                        };
                        PatternKind::Ident {
                            name,
                            mutable: false,
                            sub,
                        }
                    }
                    _ => PatternKind::Path(path),
                }
            }
            _ => {
                let Some(lit) = self.lit_pattern()? else {
                    return self.unexpected("pattern");
                };
                let lo = Pattern {
                    kind: PatternKind::Lit(lit),
                    span: start.to(self.prev_span()),
                };
                let inclusive = match self.peek() {
                    T::DotDotEq => true,
                    T::DotDot => false,
                    _ => return Ok(lo),
                };
                self.bump();
                let hs = self.span();
                let Some(hi) = self.lit_pattern()? else {
                    return self.unexpected("literal range end");
                };
                PatternKind::Range {
                    lo: Box::new(lo),
                    hi: Box::new(Pattern {
                        kind: PatternKind::Lit(hi),
                        span: hs.to(self.prev_span()),
                    }),
                    inclusive,
                }
            }
        };
        Ok(Pattern {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_struct_pattern(&mut self, path: Path) -> PResult<PatternKind> {
        let mut rest = false;
        let fields = self.brace_list(|p| {
            if p.eat(&T::DotDot) {
                rest = true;
                return Ok(None);
            }
            let mutable = p.eat_kw(Kw::Mut);
            let name = p.expect_ident()?;
            let pat = if !mutable && p.eat(&T::Colon) {
                p.parse_pattern()?
            } else {
                Pattern {
                    span: name.span,
                    kind: PatternKind::Ident {
                        name: name.clone(),
                        mutable,
                        sub: None,
                    },
                }
            };
            Ok(Some(FieldPat { name, pat }))
        })?;
        Ok(PatternKind::Struct {
            path,
            fields: fields.into_iter().flatten().collect(),
            rest,
        })
    }

    // ----- blocks and statements -----------------------------------------

    /// Parses `{ stmts }`, recovering from errors at statement boundaries.
    pub(crate) fn parse_block(&mut self) -> PResult<Block> {
        let start = self.expect(&T::LBrace)?;
        self.with_no_struct(false, |p| {
            let mut stmts = Vec::new();
            loop {
                p.skip_semis();
                if p.at(&T::RBrace) || p.at_eof() {
                    break;
                }
                let before = p.pos;
                match p.parse_stmt() {
                    Ok(s) => {
                        let block_like = match &s.kind {
                            StmtKind::Expr(e) => e.is_block_like(),
                            StmtKind::Item(_) | StmtKind::Go(GoTarget::Block { .. }) => true,
                            _ => false,
                        };
                        stmts.push(s);
                        if !(p.at_semi() || p.at(&T::RBrace) || block_like) {
                            let _ = p.unexpected::<()>("newline or `;` after statement");
                            p.recover_stmt();
                        }
                    }
                    Err(Reported) => {
                        p.recover_stmt();
                        if p.pos == before {
                            p.bump();
                        }
                    }
                }
            }
            let end = p.expect(&T::RBrace)?;
            Ok(Block {
                stmts,
                span: start.to(end),
            })
        })
    }

    /// Whether the next tokens are `x [mut] :=` or `(a, b [mut]) :=`.
    fn at_short_decl(&self) -> bool {
        let name_end = |i: usize| -> Option<usize> {
            match self.nth(i) {
                T::Ident(_) | T::Underscore => {
                    if *self.nth(i + 1) == T::Keyword(Kw::Mut) {
                        Some(i + 2)
                    } else {
                        Some(i + 1)
                    }
                }
                _ => None,
            }
        };
        if let Some(i) = name_end(0) {
            return *self.nth(i) == T::ColonEq;
        }
        if *self.peek() != T::LParen {
            return false;
        }
        let mut i = 1;
        loop {
            let Some(j) = name_end(i) else {
                return false;
            };
            match self.nth(j) {
                T::Comma => i = j + 1,
                T::RParen => return *self.nth(j + 1) == T::ColonEq,
                _ => return false,
            }
        }
    }

    fn parse_short_name(&mut self) -> PResult<ShortName> {
        let name = if self.eat(&T::Underscore) {
            None
        } else {
            Some(self.expect_ident()?)
        };
        let mutable = self.eat_kw(Kw::Mut);
        Ok(ShortName { name, mutable })
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = if self.at_item_start() {
            StmtKind::Item(Box::new(self.parse_item()?))
        } else if self.eat_kw(Kw::Let) {
            let pat = self.parse_pattern()?;
            let ty = if self.eat(&T::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            let init = self.opt_eq_expr()?;
            StmtKind::Let { pat, ty, init }
        } else if self.at_short_decl() {
            let tuple = self.eat(&T::LParen);
            let names = if tuple {
                self.comma_list(&T::RParen, Self::parse_short_name)?
            } else {
                vec![self.parse_short_name()?]
            };
            self.expect(&T::ColonEq)?;
            let init = self.parse_expr()?;
            StmtKind::ShortDecl { names, tuple, init }
        } else if self.eat_kw(Kw::Defer) {
            StmtKind::Defer(self.parse_expr()?)
        } else if self.eat_kw(Kw::Go) {
            let is_move = self.eat_kw(Kw::Move);
            if self.at(&T::LBrace) {
                StmtKind::Go(GoTarget::Block {
                    is_move,
                    body: self.parse_block()?,
                })
            } else if is_move {
                return self.unexpected("`{` after `go move`");
            } else {
                let e = self.parse_expr()?;
                if !matches!(e.kind, ExprKind::Call(..) | ExprKind::MethodCall { .. }) {
                    return self.error_at(
                        "`go` requires a function call or a block (`go f(x)` or `go move { ... }`)",
                        e.span,
                    );
                }
                StmtKind::Go(GoTarget::Call(e))
            }
        } else {
            let e = self.parse_expr()?;
            if let Some(op) = assign_op(self.peek()) {
                self.bump();
                let value = self.parse_expr()?;
                StmtKind::Assign {
                    target: e,
                    op,
                    value,
                }
            } else if self.eat(&T::LArrow) {
                let value = self.parse_expr()?;
                StmtKind::Send { chan: e, value }
            } else if self.at(&T::ColonEq) {
                let span = self.span();
                return self.error_at(
                    "left side of `:=` must be a name, `name mut`, or a tuple of names",
                    span,
                );
            } else if self.at(&T::Colon) && matches!(e.kind, ExprKind::Path(_)) {
                let span = self.span();
                self.errors.push(
                    Diagnostic::error("type annotations require `let`", span)
                        .with_help("write `let x: T = value`, or use a typed literal with `:=`"),
                );
                return Err(Reported);
            } else {
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    // ----- expressions ----------------------------------------------------

    fn can_start_expr(&self) -> bool {
        match self.peek() {
            T::Ident(_)
            | T::Lifetime(_)
            | T::Int { .. }
            | T::Float { .. }
            | T::Str(_)
            | T::RawStr(_)
            | T::Char(_)
            | T::LParen
            | T::LBracket
            | T::Minus
            | T::Bang
            | T::Star
            | T::Amp
            | T::AndAnd
            | T::LArrow
            | T::Pipe
            | T::OrOr
            | T::DotDot
            | T::DotDotEq => true,
            T::LBrace => !self.no_struct,
            T::Keyword(k) => matches!(
                k,
                Kw::SelfValue
                    | Kw::SelfType
                    | Kw::True
                    | Kw::False
                    | Kw::If
                    | Kw::Match
                    | Kw::Loop
                    | Kw::While
                    | Kw::For
                    | Kw::Return
                    | Kw::Break
                    | Kw::Continue
                    | Kw::Move
                    | Kw::Unsafe
                    | Kw::Select
                    | Kw::Chan
            ),
            _ => false,
        }
    }

    fn range_end(&mut self) -> PResult<Option<Box<Expr>>> {
        if self.can_start_expr() {
            Ok(Some(Box::new(self.parse_bin(2)?)))
        } else {
            Ok(None)
        }
    }

    /// Parses an expression, including ranges (the loosest-binding operator).
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        let start = self.span();
        let lhs = if matches!(self.peek(), T::DotDot | T::DotDotEq) {
            None
        } else {
            Some(self.parse_bin(2)?)
        };
        if !matches!(self.peek(), T::DotDot | T::DotDotEq) {
            return Ok(lhs.expect("non-range expression"));
        }
        let inclusive = self.bump().kind == T::DotDotEq;
        let end = self.range_end()?;
        Ok(Expr {
            kind: ExprKind::Range {
                start: lhs.map(Box::new),
                end,
                inclusive,
            },
            span: start.to(self.prev_span()),
        })
    }

    fn parse_bin(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            if self.at_kw(Kw::As) && CAST_PREC >= min {
                self.bump();
                let ty = self.parse_type()?;
                let span = lhs.span.to(ty.span);
                lhs = Expr {
                    kind: ExprKind::Cast(Box::new(lhs), ty),
                    span,
                };
                continue;
            }
            let Some((op, prec)) = binop(self.peek()) else {
                return Ok(lhs);
            };
            if prec < min {
                return Ok(lhs);
            }
            self.bump();
            self.skip_implicit_semis();
            let rhs = self.parse_bin(prec + 1)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let op = match self.peek() {
            T::Minus => UnOp::Neg,
            T::Bang => UnOp::Not,
            T::Star => UnOp::Deref,
            T::LArrow => UnOp::Recv,
            T::Amp | T::AndAnd => {
                let double = self.bump().kind == T::AndAnd;
                let op = if self.eat_kw(Kw::Mut) {
                    UnOp::RefMut
                } else {
                    UnOp::Ref
                };
                let inner = self.parse_unary()?;
                let mut e = Expr {
                    span: start.to(inner.span),
                    kind: ExprKind::Unary(op, Box::new(inner)),
                };
                if double {
                    e = Expr {
                        span: e.span,
                        kind: ExprKind::Unary(UnOp::Ref, Box::new(e)),
                    };
                }
                return Ok(e);
            }
            _ => {
                let prim = self.parse_primary()?;
                return self.parse_postfix(prim);
            }
        };
        self.bump();
        let inner = self.parse_unary()?;
        Ok(Expr {
            span: start.to(inner.span),
            kind: ExprKind::Unary(op, Box::new(inner)),
        })
    }

    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        self.with_no_struct(false, |p| p.comma_list(&T::RParen, Self::parse_expr))
    }

    fn parse_postfix(&mut self, mut e: Expr) -> PResult<Expr> {
        loop {
            let start = e.span;
            let kind = match self.peek().clone() {
                T::Question => {
                    self.bump();
                    ExprKind::Try(Box::new(e))
                }
                T::LParen => {
                    self.bump();
                    ExprKind::Call(Box::new(e), self.call_args()?)
                }
                T::LBracket => {
                    self.bump();
                    let idx = self.with_no_struct(false, Self::parse_expr)?;
                    self.expect(&T::RBracket)?;
                    ExprKind::Index(Box::new(e), Box::new(idx))
                }
                T::Dot => {
                    self.bump();
                    match self.peek().clone() {
                        T::Int {
                            value,
                            suffix: None,
                        } => {
                            self.bump();
                            ExprKind::TupleIndex(Box::new(e), value as u32)
                        }
                        // `t.0.1` lexes as `t` `.` `0.1`.
                        T::Float {
                            text, suffix: None, ..
                        } if text.split('.').count() == 2
                            && text.split('.').all(|p| {
                                !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())
                            }) =>
                        {
                            let sp = self.bump().span;
                            let mut parts = text.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
                            let first = Expr {
                                kind: ExprKind::TupleIndex(Box::new(e), parts.next().unwrap()),
                                span: start.to(sp),
                            };
                            ExprKind::TupleIndex(Box::new(first), parts.next().unwrap())
                        }
                        T::Keyword(Kw::Await) => {
                            let span = self.span();
                            return self.error_at(
                                "`await` is reserved; Kiln uses `go` and channels for concurrency",
                                span,
                            );
                        }
                        _ => {
                            let method = self.expect_ident()?;
                            let mut generic_args = Vec::new();
                            if self.at(&T::ColonColon) && *self.nth(1) == T::Lt {
                                self.bump();
                                generic_args = self.parse_generic_args()?;
                            }
                            if self.eat(&T::LParen) {
                                ExprKind::MethodCall {
                                    receiver: Box::new(e),
                                    method,
                                    generic_args,
                                    args: self.call_args()?,
                                }
                            } else if !generic_args.is_empty() {
                                return self.unexpected("`(` after method turbofish");
                            } else {
                                ExprKind::Field(Box::new(e), method)
                            }
                        }
                    }
                }
                _ => return Ok(e),
            };
            e = Expr {
                kind,
                span: start.to(self.prev_span()),
            };
        }
    }

    /// Parses a path in expression or pattern position; generics need `::<T>`.
    fn parse_expr_path(&mut self) -> PResult<Path> {
        let start = self.span();
        let mut segments = vec![PathSegment {
            ident: self.path_ident()?,
            args: vec![],
        }];
        while self.eat(&T::ColonColon) {
            if self.at(&T::Lt) {
                let args = self.parse_generic_args()?;
                segments.last_mut().unwrap().args = args;
            } else {
                segments.push(PathSegment {
                    ident: self.path_ident()?,
                    args: vec![],
                });
            }
        }
        Ok(Path {
            segments,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_label(&mut self) -> Option<Ident> {
        if let T::Lifetime(name) = self.peek().clone() {
            let span = self.bump().span;
            Some(Ident { name, span })
        } else {
            None
        }
    }

    fn opt_value(&mut self) -> PResult<Option<Box<Expr>>> {
        if self.can_start_expr() {
            Ok(Some(Box::new(self.parse_expr()?)))
        } else {
            Ok(None)
        }
    }

    fn literal(&mut self) -> Option<ExprKind> {
        let kind = match self.peek().clone() {
            T::Int { value, suffix } => ExprKind::Lit(Lit::Int {
                value,
                suffix,
                negative: false,
            }),
            T::Float { value, suffix, .. } => ExprKind::Lit(Lit::Float { value, suffix }),
            T::RawStr(s) => ExprKind::Lit(Lit::Str(s)),
            T::Char(c) => ExprKind::Lit(Lit::Char(c)),
            T::Keyword(Kw::True) => ExprKind::Lit(Lit::Bool(true)),
            T::Keyword(Kw::False) => ExprKind::Lit(Lit::Bool(false)),
            T::Str(parts) => match parts.as_slice() {
                [StrPart::Text(s)] => ExprKind::Lit(Lit::Str(s.clone())),
                _ => ExprKind::Interp(self.parse_interp(&parts)),
            },
            _ => return None,
        };
        self.bump();
        Some(kind)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        if let Some(kind) = self.literal() {
            return Ok(Expr { kind, span: start });
        }
        let kind = match self.peek().clone() {
            T::Ident(_) | T::Keyword(Kw::SelfValue | Kw::SelfType) => {
                let path = self.parse_expr_path()?;
                if self.at(&T::LBrace) && !self.no_struct {
                    return self.parse_struct_lit(path);
                }
                ExprKind::Path(path)
            }
            T::Keyword(Kw::Chan) if *self.nth(1) == T::ColonColon => {
                ExprKind::Path(self.parse_expr_path()?)
            }
            T::LParen => {
                self.bump();
                self.with_no_struct(false, |p| -> PResult<ExprKind> {
                    p.skip_implicit_semis();
                    if p.eat(&T::RParen) {
                        return Ok(ExprKind::Tuple(vec![]));
                    }
                    let first = p.parse_expr()?;
                    p.skip_implicit_semis();
                    if p.eat(&T::RParen) {
                        return Ok(ExprKind::Paren(Box::new(first)));
                    }
                    p.expect(&T::Comma)?;
                    let mut elems = vec![first];
                    elems.extend(p.comma_list(&T::RParen, Self::parse_expr)?);
                    Ok(ExprKind::Tuple(elems))
                })?
            }
            T::LBracket => {
                self.bump();
                self.with_no_struct(false, |p| -> PResult<ExprKind> {
                    p.skip_implicit_semis();
                    if p.eat(&T::RBracket) {
                        return Ok(ExprKind::Array(vec![]));
                    }
                    let first = p.parse_expr()?;
                    if p.eat(&T::Semi { implicit: false }) {
                        let n = p.parse_expr()?;
                        p.expect(&T::RBracket)?;
                        return Ok(ExprKind::ArrayRepeat(Box::new(first), Box::new(n)));
                    }
                    p.skip_implicit_semis();
                    if p.eat(&T::RBracket) {
                        return Ok(ExprKind::Array(vec![first]));
                    }
                    p.expect(&T::Comma)?;
                    let mut elems = vec![first];
                    elems.extend(p.comma_list(&T::RBracket, Self::parse_expr)?);
                    Ok(ExprKind::Array(elems))
                })?
            }
            T::LBrace => ExprKind::Block(self.parse_block()?),
            T::Keyword(Kw::Unsafe) => {
                self.bump();
                ExprKind::Unsafe(self.parse_block()?)
            }
            T::Keyword(Kw::If) => return self.parse_if(),
            T::Keyword(Kw::Match) => self.parse_match()?,
            T::Keyword(Kw::Select) => self.parse_select()?,
            T::Lifetime(_) if *self.nth(1) == T::Colon => {
                let label = self.parse_label();
                self.bump();
                if !matches!(self.peek(), T::Keyword(Kw::For | Kw::While | Kw::Loop)) {
                    return self.unexpected("`for`, `while`, or `loop` after label");
                }
                self.parse_loop(label)?
            }
            T::Keyword(Kw::For | Kw::While | Kw::Loop) => self.parse_loop(None)?,
            T::Keyword(Kw::Return) => {
                self.bump();
                ExprKind::Return(self.opt_value()?)
            }
            T::Keyword(Kw::Break) => {
                self.bump();
                let label = self.parse_label();
                ExprKind::Break(label, self.opt_value()?)
            }
            T::Keyword(Kw::Continue) => {
                self.bump();
                ExprKind::Continue(self.parse_label())
            }
            T::Pipe | T::OrOr | T::Keyword(Kw::Move) => self.parse_closure()?,
            T::Keyword(k) if k.is_reserved() => {
                let span = self.span();
                return self.error_at(format!("`{}` is a reserved keyword", k.as_str()), span);
            }
            _ => return self.unexpected("expression"),
        };
        Ok(Expr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    /// Parses the `{expr}` parts of an interpolated string with a sub-parser.
    fn parse_interp(&mut self, parts: &[StrPart]) -> Vec<InterpPart> {
        let mut out = Vec::new();
        for part in parts {
            match part {
                StrPart::Text(s) => out.push(InterpPart::Text(s.clone())),
                StrPart::Interp { src, offset } => {
                    let lexed = lex_at(src, *offset);
                    self.errors.extend(lexed.errors);
                    let mut sub = Parser::new(self.src, lexed.tokens, Vec::new());
                    let expr = match sub.parse_expr() {
                        Ok(e) => {
                            sub.skip_semis();
                            if !sub.at_eof() {
                                let _ = sub.unexpected::<()>("end of interpolated expression");
                            }
                            e
                        }
                        Err(Reported) => Expr {
                            kind: ExprKind::Error,
                            span: Span::new(*offset, offset + src.len()),
                        },
                    };
                    self.errors.extend(sub.errors);
                    out.push(InterpPart::Expr(expr));
                }
            }
        }
        out
    }

    fn parse_struct_lit(&mut self, path: Path) -> PResult<Expr> {
        let start = path.span;
        let mut base = None;
        let fields = self.brace_list(|p| {
            if p.eat(&T::DotDot) {
                base = Some(Box::new(p.parse_expr()?));
                return Ok(None);
            }
            let name = p.expect_ident()?;
            let value = if p.eat(&T::Colon) {
                p.parse_expr()?
            } else {
                Expr {
                    span: name.span,
                    kind: ExprKind::Path(Path::single(name.clone())),
                }
            };
            Ok(Some(FieldInit { name, value }))
        })?;
        Ok(Expr {
            kind: ExprKind::StructLit {
                path,
                fields: fields.into_iter().flatten().collect(),
                base,
            },
            span: start.to(self.prev_span()),
        })
    }

    /// Parses a condition/scrutinee, where `Path {` starts the block instead of a struct literal.
    fn parse_cond(&mut self) -> PResult<Expr> {
        self.with_no_struct(true, Self::parse_expr)
    }

    fn parse_if(&mut self) -> PResult<Expr> {
        let start = self.expect_kw(Kw::If)?;
        let kind = if self.eat_kw(Kw::Let) {
            let pat = self.parse_pattern()?;
            self.expect(&T::Eq)?;
            let expr = Box::new(self.parse_cond()?);
            let then = self.parse_block()?;
            let els = self.parse_else()?;
            ExprKind::IfLet {
                pat,
                expr,
                then,
                els,
            }
        } else {
            let cond = Box::new(self.parse_cond()?);
            let then = self.parse_block()?;
            let els = self.parse_else()?;
            ExprKind::If { cond, then, els }
        };
        Ok(Expr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_else(&mut self) -> PResult<Option<Box<Expr>>> {
        if !self.eat_kw(Kw::Else) {
            return Ok(None);
        }
        if self.at_kw(Kw::If) {
            return Ok(Some(Box::new(self.parse_if()?)));
        }
        let b = self.parse_block()?;
        Ok(Some(Box::new(Expr {
            span: b.span,
            kind: ExprKind::Block(b),
        })))
    }

    fn parse_match(&mut self) -> PResult<ExprKind> {
        self.expect_kw(Kw::Match)?;
        let scrutinee = self.parse_cond()?;
        let arms = self.with_no_struct(false, |p| {
            p.brace_list(|p| {
                let pat = p.parse_pattern()?;
                let guard = if p.eat_kw(Kw::If) {
                    Some(p.parse_expr()?)
                } else {
                    None
                };
                p.expect(&T::FatArrow)?;
                let body = p.parse_expr()?;
                Ok(MatchArm {
                    span: pat.span.to(body.span),
                    pat,
                    guard,
                    body,
                })
            })
        })?;
        Ok(ExprKind::Match(Box::new(scrutinee), arms))
    }

    fn parse_select(&mut self) -> PResult<ExprKind> {
        self.expect_kw(Kw::Select)?;
        let arms = self.with_no_struct(false, |p| {
            p.brace_list(|p| {
                let start = p.span();
                let op = if p.at(&T::Underscore) && *p.nth(1) == T::FatArrow {
                    p.bump();
                    SelectOp::Default
                } else if p.at_short_decl() {
                    let bind = p.parse_short_name()?;
                    p.expect(&T::ColonEq)?;
                    p.expect(&T::LArrow)?;
                    SelectOp::Recv {
                        bind: Some(bind),
                        chan: p.parse_expr()?,
                    }
                } else if p.eat(&T::LArrow) {
                    SelectOp::Recv {
                        bind: None,
                        chan: p.parse_expr()?,
                    }
                } else {
                    let chan = p.parse_expr()?;
                    if !p.eat(&T::LArrow) {
                        return p.unexpected(
                            "`<-` in select arm (`<-rx`, `v := <-rx`, `tx <- v`, or `_`)",
                        );
                    }
                    SelectOp::Send {
                        chan,
                        value: p.parse_expr()?,
                    }
                };
                p.expect(&T::FatArrow)?;
                let body = p.parse_expr()?;
                Ok(SelectArm {
                    op,
                    span: start.to(body.span),
                    body,
                })
            })
        })?;
        Ok(ExprKind::Select(arms))
    }

    fn parse_loop(&mut self, label: Option<Ident>) -> PResult<ExprKind> {
        Ok(match self.bump().kind {
            T::Keyword(Kw::Loop) => ExprKind::Loop {
                label,
                body: self.parse_block()?,
            },
            T::Keyword(Kw::While) if self.eat_kw(Kw::Let) => {
                let pat = self.parse_pattern()?;
                self.expect(&T::Eq)?;
                let expr = Box::new(self.parse_cond()?);
                ExprKind::WhileLet {
                    label,
                    pat,
                    expr,
                    body: self.parse_block()?,
                }
            }
            T::Keyword(Kw::While) => {
                let cond = Box::new(self.parse_cond()?);
                ExprKind::While {
                    label,
                    cond,
                    body: self.parse_block()?,
                }
            }
            _ => {
                let pat = self.parse_pattern()?;
                self.expect_kw(Kw::In)?;
                let iter = Box::new(self.parse_cond()?);
                ExprKind::For {
                    label,
                    pat,
                    iter,
                    body: self.parse_block()?,
                }
            }
        })
    }

    fn parse_closure(&mut self) -> PResult<ExprKind> {
        let is_move = self.eat_kw(Kw::Move);
        let params = if self.eat(&T::OrOr) {
            Vec::new()
        } else {
            self.expect(&T::Pipe)?;
            self.comma_list(&T::Pipe, |p| {
                let pat = p.parse_pattern_no_or()?;
                let ty = if p.eat(&T::Colon) {
                    Some(p.parse_type()?)
                } else {
                    None
                };
                Ok(ClosureParam { pat, ty })
            })?
        };
        let ret = if self.eat(&T::Arrow) {
            let t = self.parse_type()?;
            if !self.at(&T::LBrace) {
                return self.unexpected("`{` (closures with a return type need a block body)");
            }
            Some(t)
        } else {
            None
        };
        let body = Box::new(self.with_no_struct(false, Self::parse_expr)?);
        Ok(ExprKind::Closure {
            is_move,
            params,
            ret,
            body,
        })
    }
}
