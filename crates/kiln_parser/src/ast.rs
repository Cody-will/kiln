//! Abstract syntax tree for Kiln source files.
//!
//! Every node that can carry an error location has a [`Span`]. Blocks are a
//! flat list of statements: whether a block produces a value (only the arms of
//! `if`/`match` used in value position may, per SPEC §3.2) is decided by later
//! phases, not by the parser.

pub use kiln_lexer::Span;

/// An identifier with its span.
#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    /// The identifier text.
    pub name: String,
    /// Where it appears.
    pub span: Span,
}

/// A parsed source file.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    /// Top-level items in source order.
    pub items: Vec<Item>,
}

/// Item visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// No `pub`: private to the module.
    Private,
    /// `pub`: visible everywhere.
    Public,
    /// `pub(pkg)`: visible within the package.
    Package,
}

/// An attribute such as `#[test]` or `#[derive(Debug, Clone)]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// Attribute name, e.g. `derive`.
    pub name: Ident,
    /// Raw source text of the arguments inside the parentheses, if any.
    pub args: Option<String>,
    /// Span of the whole attribute.
    pub span: Span,
}

/// A top-level or nested item.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The item's kind and payload.
    pub kind: ItemKind,
    /// Visibility modifier.
    pub vis: Visibility,
    /// Attributes preceding the item.
    pub attrs: Vec<Attribute>,
    /// Doc comment text (lines joined with `\n`), if any.
    pub doc: Option<String>,
    /// Span of the whole item.
    pub span: Span,
}

/// The different kinds of items.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// `fn name<...>(...) -> T where ... { ... }`
    Fn(FnDecl),
    /// `struct`
    Struct(StructDecl),
    /// `enum`
    Enum(EnumDecl),
    /// `trait`
    Trait(TraitDecl),
    /// `impl`
    Impl(ImplDecl),
    /// `import path::{...}`
    Import(ImportTree),
    /// `const NAME: T = expr`
    Const {
        /// Name.
        name: Ident,
        /// Declared type.
        ty: Type,
        /// Value (absent in trait declarations).
        value: Option<Expr>,
    },
    /// `static [mut] NAME: T = expr`
    Static {
        /// Name.
        name: Ident,
        /// Whether declared `static mut`.
        mutable: bool,
        /// Declared type.
        ty: Type,
        /// Initializer.
        value: Expr,
    },
    /// `type Name<...> = T` (or `type Name` in traits).
    TypeAlias {
        /// Name.
        name: Ident,
        /// Generic parameters.
        generics: Generics,
        /// Aliased type (absent for associated type declarations).
        ty: Option<Type>,
    },
    /// `mod name` or `mod name { ... }`
    Mod {
        /// Module name.
        name: Ident,
        /// Inline items, or `None` for an out-of-line module.
        items: Option<Vec<Item>>,
    },
}

/// Generic parameters and where-clause.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Generics {
    /// Parameters between `<` and `>`.
    pub params: Vec<GenericParam>,
    /// `where` predicates.
    pub where_clause: Vec<WherePredicate>,
}

/// One generic parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum GenericParam {
    /// `'a`, with optional outlives bounds `'a: 'b`.
    Lifetime {
        /// Lifetime name without the quote.
        name: Ident,
        /// Bounds.
        bounds: Vec<Ident>,
    },
    /// `T: Bound + Bound`, with an optional default `= Ty`.
    Type {
        /// Name.
        name: Ident,
        /// Trait or lifetime bounds.
        bounds: Vec<Bound>,
        /// Default type.
        default: Option<Type>,
    },
    /// `const N: usize`
    Const {
        /// Name.
        name: Ident,
        /// Type of the constant.
        ty: Type,
    },
}

/// A bound in a generic parameter or where clause.
#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    /// A trait bound such as `Ord` or `Into<String>`; `maybe` is true for `?Sized`.
    Trait {
        /// The trait path.
        path: Path,
        /// `?Trait` (relaxed bound).
        maybe: bool,
    },
    /// A lifetime bound such as `'static`.
    Lifetime(Ident),
}

/// `Type: Bound + Bound` in a where clause.
#[derive(Debug, Clone, PartialEq)]
pub struct WherePredicate {
    /// For lifetime predicates (`'a: 'b + 'c`), the constrained lifetime; `ty` is then `Infer`
    /// and `bounds` are all `Bound::Lifetime`.
    pub lifetime: Option<Ident>,
    /// The constrained type.
    pub ty: Type,
    /// Bounds on it.
    pub bounds: Vec<Bound>,
    /// Span of the predicate.
    pub span: Span,
}

/// How a method takes `self`.
#[derive(Debug, Clone, PartialEq)]
pub enum SelfParam {
    /// `self` or `mut self`.
    Value {
        /// `mut self`.
        mutable: bool,
    },
    /// `&self`, `&mut self`, `&'a self`.
    Ref {
        /// Optional explicit lifetime.
        lifetime: Option<Ident>,
        /// `&mut self`.
        mutable: bool,
    },
}

/// A function parameter `pattern: Type`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Binding pattern.
    pub pat: Pattern,
    /// Declared type.
    pub ty: Type,
    /// Span.
    pub span: Span,
}

/// A function declaration (free function, method, or trait method).
#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    /// Function name.
    pub name: Ident,
    /// Whether declared `unsafe fn`.
    pub is_unsafe: bool,
    /// Generic parameters and where clause.
    pub generics: Generics,
    /// The `self` parameter, if any.
    pub self_param: Option<SelfParam>,
    /// Ordinary parameters.
    pub params: Vec<Param>,
    /// Declared return type; `None` means unit.
    pub ret: Option<Type>,
    /// Body; `None` for trait method signatures.
    pub body: Option<Block>,
}

/// A named struct field or enum struct-variant field.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    /// Visibility.
    pub vis: Visibility,
    /// Field name.
    pub name: Ident,
    /// Field type.
    pub ty: Type,
    /// Span.
    pub span: Span,
}

/// Shape of a struct or enum variant's fields.
#[derive(Debug, Clone, PartialEq)]
pub enum Fields {
    /// No fields: `struct Marker`, `None`.
    Unit,
    /// Positional fields: `Circle(f64)`.
    Tuple(Vec<Type>),
    /// Named fields: `Rect { w: f64, h: f64 }`.
    Named(Vec<FieldDecl>),
}

/// `struct Name<...> ...`
#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    /// Name.
    pub name: Ident,
    /// Generics.
    pub generics: Generics,
    /// Fields.
    pub fields: Fields,
}

/// One enum variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    /// Name.
    pub name: Ident,
    /// Payload shape.
    pub fields: Fields,
    /// Explicit discriminant `= expr`.
    pub discriminant: Option<Expr>,
    /// Span.
    pub span: Span,
}

/// `enum Name<...> { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// Name.
    pub name: Ident,
    /// Generics.
    pub generics: Generics,
    /// Variants.
    pub variants: Vec<Variant>,
}

/// `trait Name<...>: Supertraits { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    /// `unsafe trait`: implementing it requires `unsafe impl`.
    pub is_unsafe: bool,
    /// Name.
    pub name: Ident,
    /// Generics.
    pub generics: Generics,
    /// Supertrait bounds.
    pub supertraits: Vec<Bound>,
    /// Associated items (fns, types, consts).
    pub items: Vec<Item>,
}

/// `impl<...> [Trait for] Type where ... { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct ImplDecl {
    /// `unsafe impl` (required for `Send` and `Sync`).
    pub is_unsafe: bool,
    /// Generics.
    pub generics: Generics,
    /// Implemented trait, if this is a trait impl.
    pub trait_ref: Option<Path>,
    /// The type being implemented.
    pub self_ty: Type,
    /// Associated items.
    pub items: Vec<Item>,
}

/// An import tree: `a::b`, `a::b as c`, `a::*`, `a::{b, c::d}`.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportTree {
    /// A single path with optional alias.
    Path {
        /// The imported path.
        path: Vec<Ident>,
        /// `as` alias.
        alias: Option<Ident>,
    },
    /// `prefix::*`
    Glob {
        /// Prefix path.
        prefix: Vec<Ident>,
    },
    /// `prefix::{...}`
    Group {
        /// Prefix path.
        prefix: Vec<Ident>,
        /// Nested trees.
        items: Vec<ImportTree>,
    },
}

/// A path such as `std::io::Error` or `Vec<T>` or `chan::<Job>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// Segments.
    pub segments: Vec<PathSegment>,
    /// Span.
    pub span: Span,
}

/// One path segment with optional generic arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct PathSegment {
    /// Segment name (`self`, `Self`, and `chan` are allowed).
    pub ident: Ident,
    /// Generic arguments, if any.
    pub args: Vec<GenericArg>,
}

/// A generic argument.
#[derive(Debug, Clone, PartialEq)]
pub enum GenericArg {
    /// A type argument.
    Type(Type),
    /// A lifetime argument.
    Lifetime(Ident),
    /// Associated-type binding `Item = T`.
    Binding {
        /// Associated type name.
        name: Ident,
        /// Bound type.
        ty: Type,
    },
}

impl Path {
    /// Builds a simple single-segment path.
    pub fn single(ident: Ident) -> Path {
        let span = ident.span;
        Path {
            segments: vec![PathSegment {
                ident,
                args: vec![],
            }],
            span,
        }
    }

    /// Renders the path without generic arguments, e.g. `std::io::Error`.
    pub fn to_simple_string(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.ident.name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
}

/// A type expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    /// Kind of type.
    pub kind: TypeKind,
    /// Span.
    pub span: Span,
}

/// The kinds of type expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    /// A named type, possibly generic.
    Path(Path),
    /// `&'a mut T`
    Ref {
        /// Optional lifetime.
        lifetime: Option<Ident>,
        /// `&mut`.
        mutable: bool,
        /// Referent.
        inner: Box<Type>,
    },
    /// `*const T` / `*mut T`
    Ptr {
        /// `*mut`.
        mutable: bool,
        /// Pointee.
        inner: Box<Type>,
    },
    /// `[T]`
    Slice(Box<Type>),
    /// `[T; N]`
    Array(Box<Type>, Box<Expr>),
    /// `(A, B)`; `()` is the unit type.
    Tuple(Vec<Type>),
    /// `fn(A, B) -> R`
    Fn {
        /// Parameter types.
        params: Vec<Type>,
        /// Return type.
        ret: Option<Box<Type>>,
        /// `fn mut(..)`: may mutate its captures (not `Copy`, `Send`, or `Sync`).
        is_mut: bool,
    },
    /// `dyn Trait + Trait`
    Dyn(Vec<Bound>),
    /// `impl Trait`
    Impl(Vec<Bound>),
    /// `chan<- T`: send-only channel end.
    ChanSend(Box<Type>),
    /// `<-chan T`: receive-only channel end.
    ChanRecv(Box<Type>),
    /// `!`
    Never,
    /// `_`
    Infer,
}

/// A pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Kind of pattern.
    pub kind: PatternKind,
    /// Span.
    pub span: Span,
}

/// The kinds of patterns.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// `_`
    Wild,
    /// `..` inside tuple/slice patterns.
    Rest,
    /// A binding `[mut] name [@ subpattern]`. Name resolution decides whether a
    /// lone identifier like `None` is actually a unit variant.
    Ident {
        /// Bound name.
        name: Ident,
        /// `mut name`.
        mutable: bool,
        /// `name @ pat`.
        sub: Option<Box<Pattern>>,
    },
    /// A literal (optionally negated number).
    Lit(Lit),
    /// `lo..=hi`, `lo..hi`
    Range {
        /// Lower bound.
        lo: Box<Pattern>,
        /// Upper bound.
        hi: Box<Pattern>,
        /// `..=`.
        inclusive: bool,
    },
    /// `(a, b)`
    Tuple(Vec<Pattern>),
    /// `[a, b, ..]`
    Slice(Vec<Pattern>),
    /// A multi-segment path: `Shape::Empty`.
    Path(Path),
    /// `Some(x)`
    TupleStruct(Path, Vec<Pattern>),
    /// `Rect { w, h: height, .. }`
    Struct {
        /// Struct or variant path.
        path: Path,
        /// Field patterns.
        fields: Vec<FieldPat>,
        /// Trailing `..`.
        rest: bool,
    },
    /// `&pat` / `&mut pat`
    Ref {
        /// `&mut`.
        mutable: bool,
        /// Inner pattern.
        inner: Box<Pattern>,
    },
    /// `a | b`
    Or(Vec<Pattern>),
}

/// A field pattern `name: pat` (or shorthand `name`).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldPat {
    /// Field name.
    pub name: Ident,
    /// Pattern (for shorthand, an `Ident` pattern with the same name).
    pub pat: Pattern,
}

/// A `{ ... }` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Statements.
    pub stmts: Vec<Stmt>,
    /// Span including braces.
    pub span: Span,
}

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    /// Kind of statement.
    pub kind: StmtKind,
    /// Span.
    pub span: Span,
}

/// One name in a short declaration (`x`, `x mut`, or `_`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShortName {
    /// The name; `None` for `_`.
    pub name: Option<Ident>,
    /// `x mut`.
    pub mutable: bool,
}

/// Target of a `go` expression.
#[derive(Debug, Clone, PartialEq)]
pub enum GoTarget {
    /// `go f(args)` / `go obj.method(args)`.
    Call(Expr),
    /// `go [move] { ... }`
    Block {
        /// `go move { ... }`.
        is_move: bool,
        /// Task body.
        body: Block,
    },
}

/// The kinds of statements.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `let [mut] pat [: T] = expr` (the `mut` is part of the pattern).
    Let {
        /// Binding pattern.
        pat: Pattern,
        /// Optional type annotation.
        ty: Option<Type>,
        /// Initializer (`let x: T` without one is allowed).
        init: Option<Expr>,
    },
    /// `x := e`, `x mut := e`, `(a, b mut) := e`
    ShortDecl {
        /// Declared names.
        names: Vec<ShortName>,
        /// Whether the left side was parenthesized (tuple destructuring).
        tuple: bool,
        /// Initializer.
        init: Expr,
    },
    /// `place = expr` or compound assignment `place += expr`.
    Assign {
        /// Assigned place.
        target: Expr,
        /// Operator; `None` for plain `=`.
        op: Option<BinOp>,
        /// Value.
        value: Expr,
    },
    /// `tx <- value`
    Send {
        /// Channel (send end).
        chan: Expr,
        /// Value sent.
        value: Expr,
    },
    /// `defer expr` / `defer { ... }`
    Defer(Expr),
    /// An expression statement.
    Expr(Expr),
    /// A nested item.
    Item(Box<Item>),
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// Kind of expression.
    pub kind: ExprKind,
    /// Span.
    pub span: Span,
}

/// A literal.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// Integer with optional suffix; `negative` is set for negated literal patterns.
    Int {
        /// Magnitude.
        value: u128,
        /// Type suffix.
        suffix: Option<String>,
        /// Leading `-` (patterns only).
        negative: bool,
    },
    /// Float with optional suffix.
    Float {
        /// Value.
        value: f64,
        /// Type suffix.
        suffix: Option<String>,
    },
    /// Non-interpolated string (including raw strings).
    Str(String),
    /// Character.
    Char(char),
    /// Boolean.
    Bool(bool),
}

/// Part of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    /// Literal text.
    Text(String),
    /// An embedded expression, with its format spec (`{x:>8.2}`), if any.
    Expr(Expr, Option<FmtSpec>),
}

/// How an interpolated value is formatted: `{x:[[fill]align][+][#][0][width][.precision][kind]}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FmtSpec {
    /// Padding character (default space).
    pub fill: Option<char>,
    /// `<` left, `^` center, `>` right (default: right for numbers, left otherwise).
    pub align: Option<FmtAlign>,
    /// `+`: always print the sign of numbers.
    pub plus: bool,
    /// `#`: alternate form (`0x` prefixes, pretty `{:#?}`).
    pub alt: bool,
    /// `0`: pad numbers with zeros after the sign.
    pub zero: bool,
    /// Minimum width in characters.
    pub width: Option<usize>,
    /// Digits after the point (floats) or maximum characters (strings).
    pub precision: Option<usize>,
    /// Which representation.
    pub kind: FmtKind,
}

/// Alignment within the width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum FmtAlign {
    Left,
    Center,
    Right,
}

/// The representation a format spec asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FmtKind {
    /// `{x}`: `Display`.
    #[default]
    Display,
    /// `{x:?}`: `Debug`.
    Debug,
    /// `{x:x}` / `{x:X}`: hexadecimal.
    LowerHex,
    /// Uppercase hexadecimal.
    UpperHex,
    /// `{x:o}`: octal.
    Octal,
    /// `{x:b}`: binary.
    Binary,
    /// `{x:e}` / `{x:E}`: scientific notation.
    LowerExp,
    /// Uppercase `E` scientific notation.
    UpperExp,
}

impl FmtSpec {
    /// Parses the text after `:` in `{x:spec}`.
    pub fn parse(s: &str) -> Result<FmtSpec, String> {
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0;
        let mut f = FmtSpec::default();
        let align = |c: char| match c {
            '<' => Some(FmtAlign::Left),
            '^' => Some(FmtAlign::Center),
            '>' => Some(FmtAlign::Right),
            _ => None,
        };
        if cs.len() >= 2 && align(cs[1]).is_some() {
            f.fill = Some(cs[0]);
            f.align = align(cs[1]);
            i = 2;
        } else if let Some(a) = cs.first().and_then(|c| align(*c)) {
            f.align = Some(a);
            i = 1;
        }
        if cs.get(i) == Some(&'+') {
            f.plus = true;
            i += 1;
        }
        if cs.get(i) == Some(&'#') {
            f.alt = true;
            i += 1;
        }
        if cs.get(i) == Some(&'0') {
            f.zero = true;
            i += 1;
        }
        let num = |i: &mut usize| -> Option<usize> {
            let st = *i;
            while *i < cs.len() && cs[*i].is_ascii_digit() {
                *i += 1;
            }
            (*i > st).then(|| {
                cs[st..*i]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(usize::MAX)
            })
        };
        f.width = num(&mut i);
        if cs.get(i) == Some(&'.') {
            i += 1;
            f.precision = num(&mut i);
            if f.precision.is_none() {
                return Err("expected a number after `.` in format spec".into());
            }
        }
        f.kind = match cs.get(i) {
            None => FmtKind::Display,
            Some('?') => FmtKind::Debug,
            Some('x') => FmtKind::LowerHex,
            Some('X') => FmtKind::UpperHex,
            Some('o') => FmtKind::Octal,
            Some('b') => FmtKind::Binary,
            Some('e') => FmtKind::LowerExp,
            Some('E') => FmtKind::UpperExp,
            Some(c) => {
                return Err(format!(
                    "unknown format `{c}` (expected one of `?`, `x`, `X`, `o`, `b`, `e`, `E`)"
                ))
            }
        };
        if f.kind != FmtKind::Display {
            i += 1;
        }
        if i != cs.len() {
            return Err(format!("invalid format spec `{s}`; the form is `[[fill]align][+][#][0][width][.precision][?|x|X|o|b|e|E]`"));
        }
        Ok(f)
    }
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `!x`
    Not,
    /// `*x`
    Deref,
    /// `&x`
    Ref,
    /// `&mut x`
    RefMut,
    /// `<-rx` (channel receive)
    Recv,
}

/// A closure parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureParam {
    /// Pattern.
    pub pat: Pattern,
    /// Optional type annotation.
    pub ty: Option<Type>,
}

/// One `match` arm.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// Pattern.
    pub pat: Pattern,
    /// `if guard`.
    pub guard: Option<Expr>,
    /// Arm body.
    pub body: Expr,
    /// Span.
    pub span: Span,
}

/// The operation in a `select` arm.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectOp {
    /// `[name [mut] :=] <-rx`
    Recv {
        /// Binding for the received value.
        bind: Option<ShortName>,
        /// Receive end.
        chan: Expr,
    },
    /// `tx <- value`
    Send {
        /// Send end.
        chan: Expr,
        /// Value.
        value: Expr,
    },
    /// `_` (runs if no other arm is ready).
    Default,
}

/// One `select` arm.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectArm {
    /// Operation.
    pub op: SelectOp,
    /// Body.
    pub body: Expr,
    /// Span.
    pub span: Span,
}

/// A struct literal field `name: expr` or shorthand `name`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    /// Field name.
    pub name: Ident,
    /// Value (for shorthand, a path expression with the same name).
    pub value: Expr,
}

/// The kinds of expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A literal.
    Lit(Lit),
    /// An interpolated string `"a {b} c"`.
    Interp(Vec<InterpPart>),
    /// A path: `x`, `Shape::Circle`, `chan::<Job>`.
    Path(Path),
    /// Unary operation.
    Unary(UnOp, Box<Expr>),
    /// Binary operation.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `e as T`
    Cast(Box<Expr>, Type),
    /// `f(args)`
    Call(Box<Expr>, Vec<Expr>),
    /// `recv.method::<T>(args)`
    MethodCall {
        /// Receiver.
        receiver: Box<Expr>,
        /// Method name.
        method: Ident,
        /// Turbofish arguments.
        generic_args: Vec<GenericArg>,
        /// Arguments.
        args: Vec<Expr>,
    },
    /// `e.field`
    Field(Box<Expr>, Ident),
    /// `e.0`
    TupleIndex(Box<Expr>, u32),
    /// `e[i]`
    Index(Box<Expr>, Box<Expr>),
    /// `e?`
    Try(Box<Expr>),
    /// `a..b`, `a..`, `..b`, `..`, `a..=b`
    Range {
        /// Start.
        start: Option<Box<Expr>>,
        /// End.
        end: Option<Box<Expr>>,
        /// `..=`.
        inclusive: bool,
    },
    /// `Path { field: value, ..base }`
    StructLit {
        /// Struct path.
        path: Path,
        /// Fields.
        fields: Vec<FieldInit>,
        /// Functional update base.
        base: Option<Box<Expr>>,
    },
    /// `(a, b)`; `()` is unit.
    Tuple(Vec<Expr>),
    /// `(e)`
    Paren(Box<Expr>),
    /// `[a, b]`
    Array(Vec<Expr>),
    /// `[v; n]`
    ArrayRepeat(Box<Expr>, Box<Expr>),
    /// `{ ... }` (optionally labeled)
    Block(Block),
    /// `unsafe { ... }`
    Unsafe(Block),
    /// `go f(args)` or `go [move] { ... }`: starts a task, giving a `Task<T>`.
    Go(Box<GoTarget>),
    /// `if cond { } else ...`
    If {
        /// Condition.
        cond: Box<Expr>,
        /// Then branch.
        then: Block,
        /// Else branch: a block or another `if`.
        els: Option<Box<Expr>>,
    },
    /// `if let pat = expr { } else ...`
    IfLet {
        /// Pattern.
        pat: Pattern,
        /// Scrutinee.
        expr: Box<Expr>,
        /// Then branch.
        then: Block,
        /// Else branch.
        els: Option<Box<Expr>>,
    },
    /// `match e { arms }`
    Match(Box<Expr>, Vec<MatchArm>),
    /// `select { arms }`
    Select(Vec<SelectArm>),
    /// `['label:] for pat in iter { }`
    For {
        /// Optional label.
        label: Option<Ident>,
        /// Loop pattern.
        pat: Pattern,
        /// Iterated expression.
        iter: Box<Expr>,
        /// Body.
        body: Block,
    },
    /// `['label:] while cond { }`
    While {
        /// Optional label.
        label: Option<Ident>,
        /// Condition.
        cond: Box<Expr>,
        /// Body.
        body: Block,
    },
    /// `['label:] while let pat = e { }`
    WhileLet {
        /// Optional label.
        label: Option<Ident>,
        /// Pattern.
        pat: Pattern,
        /// Scrutinee.
        expr: Box<Expr>,
        /// Body.
        body: Block,
    },
    /// `['label:] loop { }`
    Loop {
        /// Optional label.
        label: Option<Ident>,
        /// Body.
        body: Block,
    },
    /// `break ['label] [value]`
    Break(Option<Ident>, Option<Box<Expr>>),
    /// `continue ['label]`
    Continue(Option<Ident>),
    /// `return [value]`
    Return(Option<Box<Expr>>),
    /// `[move] |params| [-> T] body`
    Closure {
        /// `move` closure.
        is_move: bool,
        /// Parameters.
        params: Vec<ClosureParam>,
        /// Explicit return type.
        ret: Option<Type>,
        /// Body.
        body: Box<Expr>,
    },
    /// Placeholder produced after a parse error.
    Error,
}

impl Expr {
    /// Whether the expression ends with a block, so it can stand as a statement
    /// without a terminator.
    pub fn is_block_like(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::Block(_)
                | ExprKind::Unsafe(_)
                | ExprKind::If { .. }
                | ExprKind::IfLet { .. }
                | ExprKind::Match(..)
                | ExprKind::Select(_)
                | ExprKind::For { .. }
                | ExprKind::While { .. }
                | ExprKind::WhileLet { .. }
                | ExprKind::Loop { .. }
                | ExprKind::Go(_)
        )
    }
}
