//! Name resolution for Kiln.
//!
//! [`resolve`] walks a parsed file (plus the built-in prelude) and produces a
//! [`Resolved`] table that later phases query by source span:
//!
//! - every item becomes a [`Def`] with a [`DefId`]; modules have separate type
//!   and value namespaces;
//! - `import`s are bound in the importing module (paths starting with `std` or
//!   `core` are treated as external, opaque items until the standard library exists);
//! - every local binding (`let`, `:=`, parameters, patterns, closures) gets a
//!   [`LocalId`], and every path in an expression, pattern, or type is mapped to a [`Res`];
//! - scope errors are reported: undefined names, duplicate definitions,
//!   unresolved imports, private items, unknown labels, `break` outside loops.
//!
//! Spans are unique per AST node within a file, so they are used as keys.

mod scope;
pub use scope::{kind_name, static_lifetimes};
#[cfg(test)]
mod tests;

use kiln_lexer::Diagnostic;
use kiln_parser::ast::*;
use std::collections::HashMap;

/// Source of the core prelude (types and traits visible in every module).
pub const PRELUDE_SRC: &str = include_str!("prelude.kn");

/// The standard library: `(module path, source)`, parents before children.
/// Each module becomes `pub mod <name> { ... }` inside `std`, which lives in
/// the prelude.
pub const STD_MODULES: &[(&str, &str)] = &[
    ("mem", include_str!("../std/mem.kn")),
    ("iter", include_str!("../std/iter.kn")),
    ("unicode", include_str!("../std/unicode.kn")),
    ("utf8", include_str!("../std/utf8.kn")),
    ("strings", include_str!("../std/strings.kn")),
    ("cmp", include_str!("../std/cmp.kn")),
    ("slice", include_str!("../std/slice.kn")),
    ("num", include_str!("../std/num.kn")),
    ("strconv", include_str!("../std/strconv.kn")),
    ("hash", include_str!("../std/hash.kn")),
    ("ptr", include_str!("../std/ptr.kn")),
    ("time", include_str!("../std/time.kn")),
    ("sync", include_str!("../std/sync.kn")),
    ("fmt", include_str!("../std/fmt.kn")),
    ("io", include_str!("../std/io.kn")),
    ("path", include_str!("../std/path.kn")),
    ("fs", include_str!("../std/fs.kn")),
    ("os", include_str!("../std/os.kn")),
    ("net", include_str!("../std/net.kn")),
    ("net::http", include_str!("../std/http.kn")),
    ("log", include_str!("../std/log.kn")),
];

/// The prelude and standard library as one source text, plus each file's
/// `(offset, display name, text)`.
pub struct BuiltinSource {
    /// The combined source, parsed at [`kiln_lexer::BUILTIN_BASE`].
    pub text: String,
    /// Where each file starts within `text`.
    pub files: Vec<(usize, &'static str, &'static str)>,
}

/// The combined prelude and standard library source (built once).
pub fn builtin() -> &'static BuiltinSource {
    static B: std::sync::OnceLock<BuiltinSource> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        let mut text = String::new();
        let mut files = vec![(0, "<prelude>", PRELUDE_SRC)];
        text.push_str(PRELUDE_SRC);
        text.push_str("\npub mod std {\n");
        // Emit modules as a tree: open/close `pub mod` blocks along each path.
        let mut open: Vec<&str> = vec![];
        for (path, src) in STD_MODULES {
            let segs: Vec<&str> = path.split("::").collect();
            let common = open.iter().zip(&segs).take_while(|(a, b)| a == b).count();
            while open.len() > common {
                open.pop();
                text.push_str("}\n");
            }
            for seg in &segs[common..] {
                text.push_str(&format!("pub mod {seg} {{\n"));
                open.push(seg);
            }
            let name: &'static str =
                Box::leak(format!("<std>/{}.kn", path.replace("::", "/")).into_boxed_str());
            files.push((text.len(), name, *src));
            text.push_str(src);
            text.push('\n');
        }
        for _ in open {
            text.push_str("}\n");
        }
        text.push_str("}\n");
        for (off, name, src) in &files {
            kiln_lexer::register_source(kiln_lexer::BUILTIN_BASE + off, name, src);
        }
        BuiltinSource { text, files }
    })
}

/// Parses the prelude and standard library. Panics if they fail to parse (a
/// compiler bug).
pub fn parse_prelude() -> File {
    let b = builtin();
    let p = kiln_parser::parse_at(&b.text, kiln_lexer::BUILTIN_BASE);
    if !p.errors.is_empty() {
        let msgs: String = p.errors.iter().map(|d| d.render("<builtin>", "")).collect();
        panic!("the standard library failed to parse:\n{msgs}");
    }
    p.file
}

/// Identifies a definition (item, enum variant, module, or external name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// Identifies a local variable binding within a function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// What kind of thing a [`Def`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    /// A module (the crate root or an inline `mod`).
    Mod,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// An enum variant; `index` is its position in the enum.
    Variant {
        /// The enum.
        parent: DefId,
        /// Position within the enum.
        index: usize,
    },
    /// A trait.
    Trait,
    /// A free function.
    Fn,
    /// A `const`.
    Const,
    /// A `static`.
    Static,
    /// A `type` alias.
    TypeAlias,
    /// A name imported from an external crate (`std`, `core`); opaque for now.
    External,
}

/// A definition.
#[derive(Debug, Clone)]
pub struct Def<'a> {
    /// Kind of definition.
    pub kind: DefKind,
    /// Name as written.
    pub name: String,
    /// Span of the name.
    pub span: Span,
    /// Visibility.
    pub vis: Visibility,
    /// Containing module (the root module contains itself).
    pub module: DefId,
    /// The AST item, for item definitions.
    pub item: Option<&'a Item>,
    /// The AST variant, for enum variants.
    pub variant: Option<&'a kiln_parser::ast::Variant>,
    /// Whether this comes from the built-in prelude.
    pub is_prelude: bool,
}

/// Primitive types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum PrimTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    F32,
    F64,
    Bool,
    Char,
    Str,
}

impl PrimTy {
    /// Whether this is an integer type.
    pub fn is_integer(self) -> bool {
        !matches!(
            self,
            PrimTy::F32 | PrimTy::F64 | PrimTy::Bool | PrimTy::Char | PrimTy::Str
        )
    }

    /// Looks up a primitive type by name.
    pub fn from_name(s: &str) -> Option<PrimTy> {
        use PrimTy::*;
        Some(match s {
            "i8" => I8,
            "i16" => I16,
            "i32" => I32,
            "i64" => I64,
            "i128" => I128,
            "isize" => Isize,
            "u8" => U8,
            "u16" => U16,
            "u32" => U32,
            "u64" => U64,
            "u128" => U128,
            "usize" => Usize,
            "f32" => F32,
            "f64" => F64,
            "bool" => Bool,
            "char" => Char,
            "str" => Str,
            _ => return None,
        })
    }
}

/// Compiler-provided functions that need special type-checking rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `chan::<T>(capacity)`: creates a channel, returning `(chan<- T, <-chan T)`.
    Chan,
    /// `drop(value)`: consumes a value.
    Drop,
}

/// What a path refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum Res {
    /// An item, variant, module, or external name.
    Def(DefId),
    /// A local variable.
    Local(LocalId),
    /// A primitive type.
    Prim(PrimTy),
    /// A generic type parameter in scope (by name).
    TyParam(String),
    /// `Self` inside an `impl` or `trait`.
    SelfTy,
    /// A compiler built-in function.
    Builtin(Builtin),
    /// An associated item `Base::name` (method, associated fn, or `Self::Variant`),
    /// resolved by the type checker.
    Assoc {
        /// Resolution of the base type.
        base: Box<Res>,
        /// Associated item name.
        name: String,
    },
    /// Something from an external crate; treated as opaque.
    External,
    /// Resolution failed (an error has been reported).
    Err,
}

/// A local variable.
#[derive(Debug, Clone)]
pub struct Local {
    /// Name.
    pub name: String,
    /// Declared `mut`.
    pub mutable: bool,
    /// Span of the binding.
    pub span: Span,
}

/// A function body to be type-checked, with its context.
#[derive(Debug, Clone)]
pub struct FnRef<'a> {
    /// The declaration.
    pub decl: &'a FnDecl,
    /// The item the declaration belongs to (for attributes/visibility).
    pub item: &'a Item,
    /// Containing module.
    pub module: DefId,
    /// Index into [`Resolved::impls`] if this is a method in an `impl`.
    pub impl_idx: Option<usize>,
    /// The trait, if this is a method declared in a `trait`.
    pub trait_def: Option<DefId>,
    /// The definition, for free functions.
    pub def: Option<DefId>,
    /// The local for `self`, for methods.
    pub self_local: Option<LocalId>,
    /// Whether this is a prelude declaration.
    pub is_prelude: bool,
}

/// An `impl` block.
#[derive(Debug, Clone)]
pub struct ImplRef<'a> {
    /// The declaration.
    pub decl: &'a ImplDecl,
    /// Span of the `impl` item.
    pub span: Span,
    /// Containing module.
    pub module: DefId,
    /// Whether this comes from the prelude.
    pub is_prelude: bool,
}

/// The namespaces of a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleScope {
    /// Types, traits, modules, and enums.
    pub types: HashMap<String, DefId>,
    /// Functions, constants, statics, and unit/tuple struct constructors.
    pub values: HashMap<String, DefId>,
    /// Parent module, if any.
    pub parent: Option<DefId>,
    /// Names bound by a plain (non-`pub`) `import`: visible inside this module
    /// and its children only. `(is_type_namespace, name)`.
    pub private_imports: std::collections::HashSet<(bool, String)>,
}

/// The result of name resolution.
#[derive(Debug, Clone)]
pub struct Resolved<'a> {
    /// All definitions, indexed by [`DefId`].
    pub defs: Vec<Def<'a>>,
    /// The user's root module.
    pub root: DefId,
    /// The prelude module.
    pub prelude: DefId,
    /// Namespaces of each module.
    pub modules: HashMap<DefId, ModuleScope>,
    /// Resolution of every path (keyed by the path's span).
    pub paths: HashMap<Span, Res>,
    /// Local introduced by each binding (keyed by the binding identifier's span).
    pub bindings: HashMap<Span, LocalId>,
    /// All locals, indexed by [`LocalId`].
    pub locals: Vec<Local>,
    /// All `impl` blocks.
    pub impls: Vec<ImplRef<'a>>,
    /// All function declarations (free functions, methods, trait methods).
    pub fns: Vec<FnRef<'a>>,
    /// Static items and their initializer functions (`fn <init X>() -> T`,
    /// indexes into `fns`), in declaration order.
    pub statics: Vec<(DefId, usize)>,
    /// Functions synthesized for `const` values: a use of the constant
    /// calls the function. Keyed by the constant (module-level consts).
    pub const_fns: HashMap<DefId, usize>,
    /// All such functions, including associated consts in impls (which are
    /// registered as associated functions named after the constant).
    pub const_fn_set: std::collections::HashSet<usize>,
    /// Name-resolution diagnostics.
    pub errors: Vec<Diagnostic>,
}

impl<'a> Resolved<'a> {
    /// Returns the definition for `id`.
    pub fn def(&self, id: DefId) -> &Def<'a> {
        &self.defs[id.0 as usize]
    }

    /// Returns the local for `id`.
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.0 as usize]
    }

    /// Resolution of a path, or [`Res::Err`] if it was not recorded.
    pub fn path(&self, span: Span) -> Res {
        self.paths.get(&span).cloned().unwrap_or(Res::Err)
    }

    /// Whether `d` is declared directly in the core prelude (not in `std`).
    pub fn is_core(&self, d: DefId) -> bool {
        let def = self.def(d);
        def.is_prelude && def.module == self.prelude
    }

    /// Whether function `i` is declared directly in the core prelude (not in `std`).
    pub fn fn_is_core(&self, i: usize) -> bool {
        let f = &self.fns[i];
        f.is_prelude && f.module == self.prelude
    }

    /// Looks up a type-namespace name in the prelude (e.g. `Option`).
    pub fn prelude_type(&self, name: &str) -> Option<DefId> {
        self.modules[&self.prelude].types.get(name).copied()
    }

    /// Looks up a value-namespace name in the prelude (e.g. `println`).
    pub fn prelude_value(&self, name: &str) -> Option<DefId> {
        self.modules[&self.prelude].values.get(name).copied()
    }

    /// The variants of an enum, in order.
    pub fn variants(&self, enum_id: DefId) -> Vec<DefId> {
        let mut v: Vec<(usize, DefId)> = self
            .defs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| match d.kind {
                DefKind::Variant { parent, index } if parent == enum_id => {
                    Some((index, DefId(i as u32)))
                }
                _ => None,
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, d)| d).collect()
    }
}

/// Resolves names in `file`, with `prelude` (from [`parse_prelude`]) in scope.
pub fn resolve<'a>(file: &'a File, prelude: &'a File) -> Resolved<'a> {
    scope::Resolver::run(file, prelude)
}
