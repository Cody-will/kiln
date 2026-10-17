//! The MIR data structures.
//!
//! A [`Body`] is a control-flow graph of [`BlockData`]s. Each block holds
//! [`Statement`]s and ends in a [`Terminator`]. Values live in [`Local`]s and
//! are addressed with [`Place`]s (a local plus field/deref/index projections).
//! Operands say whether a place is *copied* (read) or *moved*.
//!
//! MIR serves two clients: the borrow checker (which needs every read, move,
//! borrow, write, and end of storage, plus how borrows flow from call
//! arguments into results via [`Rvalue::Call::flows`]) and the code generator
//! (which needs constants, operators, callees with generic substitutions,
//! aggregate kinds, and conditional branches).

#![allow(missing_docs)] // field names are self-describing; see the module docs

use kiln_lexer::Span;
use kiln_resolve::{DefId, LocalId};
use kiln_typeck::ty::Ty;

/// A local slot in a body. `_0` is the return place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Local(pub u32);

/// Index of a basic block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// What a local is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// `_0`, the return value.
    Return,
    /// A parameter (including `self`).
    Arg,
    /// A user variable.
    User,
    /// A compiler temporary.
    Temp,
    /// A variable captured by a closure or `spawn` block (only in that body).
    Upvar,
}

/// A local's declaration.
#[derive(Debug, Clone)]
pub struct LocalDecl {
    pub ty: Ty,
    /// Source name for user variables, parameters, and upvars.
    pub name: Option<String>,
    pub span: Span,
    pub kind: LocalKind,
    pub mutable: bool,
    /// The resolver's id for user variables and upvars.
    pub source: Option<LocalId>,
}

/// How a deref projection dereferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerefKind {
    /// Through `&T` (`false`) or `&mut T` (`true`).
    Ref(bool),
    /// Through a raw pointer.
    Ptr,
    /// Through an owning `Box<T>`.
    Box,
}

/// [`DerefKind`] in a hashable/orderable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DerefKindKey {
    Shared,
    Mut,
    Ptr,
    Box,
}

impl From<DerefKind> for DerefKindKey {
    fn from(k: DerefKind) -> Self {
        match k {
            DerefKind::Ref(false) => DerefKindKey::Shared,
            DerefKind::Ref(true) => DerefKindKey::Mut,
            DerefKind::Ptr => DerefKindKey::Ptr,
            DerefKind::Box => DerefKindKey::Box,
        }
    }
}

/// One step of a place projection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proj {
    /// `*p`
    Deref(DerefKindKey),
    /// `p.name`, `p.0`, or `p.Variant.0` (enum fields are prefixed with the variant name).
    Field(String),
    /// `p[i]`, with the index in the given local.
    Index(Local),
}

/// A memory location: a local plus projections.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub local: Local,
    pub proj: Vec<Proj>,
}

impl Place {
    /// The whole local.
    pub fn local(l: Local) -> Place {
        Place {
            local: l,
            proj: vec![],
        }
    }

    /// This place with one more projection.
    pub fn project(&self, p: Proj) -> Place {
        let mut proj = self.proj.clone();
        proj.push(p);
        Place {
            local: self.local,
            proj,
        }
    }

    /// This place with a field projection.
    pub fn field(&self, name: impl Into<String>) -> Place {
        self.project(Proj::Field(name.into()))
    }

    /// Whether the place goes through a reference or raw pointer (so it is
    /// not storage owned by its local).
    pub fn through_ref(&self) -> bool {
        self.proj.iter().any(|p| {
            matches!(
                p,
                Proj::Deref(DerefKindKey::Shared | DerefKindKey::Mut | DerefKindKey::Ptr)
            )
        })
    }

    /// Whether one place contains the other (same local, one projection list is
    /// a prefix of the other; any two indexes may alias).
    pub fn overlaps(&self, other: &Place) -> bool {
        if self.local != other.local {
            return false;
        }
        self.proj
            .iter()
            .zip(&other.proj)
            .all(|(a, b)| a == b || matches!((a, b), (Proj::Index(_), Proj::Index(_))))
    }
}

/// A constant value.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    /// A string literal (`&'static str`).
    Str(String),
    /// A byte string literal (`&'static [u8]`).
    Bytes(Vec<u8>),
    /// `()`, or a value with no runtime representation.
    Unit,
    /// A function item used as a value, with its generic substitution.
    Fn(usize, Vec<(String, Ty)>),
}

/// A constant operand with its type.
#[derive(Debug, Clone, PartialEq)]
pub struct Constant {
    pub kind: ConstKind,
    pub ty: Ty,
}

/// A value used by an rvalue.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// Read the place (for `Copy` types, or a non-consuming read).
    Copy(Place),
    /// Move out of the place.
    Move(Place),
    /// A constant.
    Const(Box<Constant>),
}

impl Operand {
    /// The place read or moved, if any.
    pub fn place(&self) -> Option<&Place> {
        match self {
            Operand::Copy(p) | Operand::Move(p) => Some(p),
            Operand::Const(_) => None,
        }
    }

    /// The unit constant.
    pub fn unit() -> Operand {
        Operand::constant(ConstKind::Unit, Ty::Unit)
    }

    /// A constant of type `ty`.
    pub fn constant(kind: ConstKind, ty: Ty) -> Operand {
        Operand::Const(Box::new(Constant { kind, ty }))
    }
}

/// Operations whose results hold no borrows.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Bin(kiln_parser::ast::BinOp),
    Neg,
    Not,
    /// `as` cast to the given type.
    Cast(Ty),
    /// The variant index of an enum place (operand is a `copy` of the place).
    Discriminant,
    /// `print`/`println`/`eprint`/`eprintln` of one string-like operand.
    Print {
        newline: bool,
        stderr: bool,
    },
    /// String interpolation: literal segments and operand indices.
    Interp(Vec<InterpSeg>),
    /// Evaluate the operands only (a read, for the borrow checker).
    Touch,
    /// Something without a code generator yet (channels, ...).
    Unsupported(String),
}

/// One piece of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpSeg {
    Text(String),
    /// Format operand `i`, with its format spec.
    Arg(usize, Option<kiln_parser::ast::FmtSpec>),
}

/// What an aggregate builds.
#[derive(Debug, Clone, PartialEq)]
pub enum AggKind {
    Tuple,
    Array,
    /// `[x; n]`: one operand, copied `n` times.
    ArrayRepeat(u64),
    /// A struct, with the field names in operand order.
    Struct(DefId, Vec<String>),
    /// Enum variant `index` of enum `def`, with field names in operand order.
    Variant {
        def: DefId,
        index: usize,
        fields: Vec<String>,
    },
    /// `start..end` (`inclusive` for `..=`); missing bounds are omitted from the operands.
    Range {
        has_start: bool,
        has_end: bool,
        inclusive: bool,
    },
    /// A closure value; the operands are its captures. The closure's body has this span.
    Closure(Span),
}

/// What a call calls.
#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// A declared function (user or prelude) with its generic substitution.
    /// For trait methods, the implementation is chosen during monomorphization.
    Fn(usize, Vec<(String, Ty)>),
    /// A function value (closure or fn pointer): `args[0]`.
    Value,
    /// A compiler-provided operation (`clone`, `drop`, `vec_take`, ...).
    Intrinsic(String),
}

/// Flag on a call's flow index: the result holds what the argument's
/// referent holds, not the argument's own borrow (see [`crate::sig::return_flows`]).
pub const DEEP_FLOW: usize = 1 << 31;

/// The right-hand side of an assignment.
#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    /// The operand's value.
    Use(Operand),
    /// `&place` / `&mut place`.
    Ref { mutable: bool, place: Place },
    /// A struct, tuple, array, enum variant, range, or closure value built from
    /// operands. The result holds any borrows the operands hold.
    Aggregate(AggKind, Vec<Operand>),
    /// A call. The result holds the borrows held by `args[i]` for each `i` in `flows`
    /// (computed from the callee's signature and lifetime elision).
    Call {
        callee: Callee,
        args: Vec<Operand>,
        flows: Vec<usize>,
    },
    /// Arithmetic, comparisons, casts, printing: the result holds no borrows.
    Pure(Op, Vec<Operand>),
    /// An implicit coercion (`&T → &dyn Trait`, `&String → &str`, `Box<T> → Box<dyn Trait>`, ...).
    /// The result holds the operand's borrows.
    Coerce(Operand, Ty),
}

impl Rvalue {
    /// All operands.
    pub fn operands(&self) -> Vec<&Operand> {
        match self {
            Rvalue::Use(o) | Rvalue::Coerce(o, _) => vec![o],
            Rvalue::Ref { .. } => vec![],
            Rvalue::Aggregate(_, os) | Rvalue::Call { args: os, .. } | Rvalue::Pure(_, os) => {
                os.iter().collect()
            }
        }
    }
}

/// How a closure or task captures a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Ref,
    MutRef,
    Move,
}

/// A captured variable (for `Send`/`Sync` and `'static` checks).
#[derive(Debug, Clone)]
pub struct Capture {
    pub name: String,
    pub ty: Ty,
    pub mode: CaptureMode,
    /// Where the variable is used inside the closure or task.
    pub span: Span,
}

impl PartialEq for Capture {
    fn eq(&self, o: &Self) -> bool {
        self.name == o.name && self.mode == o.mode && self.span == o.span
    }
}

/// Which kind of task a [`StmtKind::Spawn`] starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    /// `spawn f(args)`: the operands are the moved arguments.
    GoCall,
    /// `spawn { ... }` / `spawn move { ... }`: the operands are the captures.
    GoBlock { is_move: bool },
    /// `s.spawn(closure)` inside `scope(...)`: may borrow locals.
    Scoped,
}

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `place = rvalue`
    Assign(Place, Rvalue),
    /// A non-consuming read of a place (match scrutinees).
    Read(Place),
    /// Evaluate an rvalue and discard the result.
    Eval(Rvalue),
    /// The local's scope ends: it is dropped and its storage invalidated.
    StorageDead(Local),
    /// Register deferred call number `n` (run at function exit, last-in first-out).
    /// With `Some(rec)`, the callee and arguments were evaluated into the tuple
    /// `rec` at the `defer` statement (Go semantics); its value moves onto the
    /// defer stack with the call.
    DeferPush(u32, Option<Local>),
    /// First statement of a deferred call's block: move the arguments saved by
    /// the matching `DeferPush` back into `rec`.
    DeferLoad(Local),
    /// Start a task.
    Spawn {
        kind: SpawnKind,
        operands: Vec<Operand>,
        captures: Vec<Capture>,
        /// Span of the task's body (a [`BodyKind::GoBlock`] body); unused for scoped tasks.
        body: Span,
    },
}

/// A statement with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub kind: StmtKind,
    pub span: Span,
}

/// How a block ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    Goto(BlockId),
    /// `if cond { goto t } else { goto f }`.
    SwitchBool {
        cond: Operand,
        t: BlockId,
        f: BlockId,
    },
    /// Branch to any of the targets with no condition the code generator knows
    /// (`select`); bodies containing it are reported as unsupported by codegen.
    Branch(Vec<BlockId>),
    Return,
    Unreachable,
    /// Pop the most recently pushed deferred call and jump to its block
    /// (`targets[n]`), or to `done` when none are left. Each target block jumps
    /// to another dispatch after running its call. `None` targets cannot be on
    /// the stack at this point (a defer outside any loop runs at most once, and
    /// only defers pushed before it can remain below it).
    DeferDispatch {
        targets: Vec<Option<BlockId>>,
        done: BlockId,
    },
}

impl Terminator {
    /// Successor blocks.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::SwitchBool { t, f, .. } => vec![*t, *f],
            Terminator::Branch(bs) => bs.clone(),
            Terminator::Return | Terminator::Unreachable => vec![],
            Terminator::DeferDispatch { targets, done } => {
                let mut v: Vec<BlockId> = targets.iter().flatten().copied().collect();
                v.push(*done);
                v
            }
        }
    }
}

/// A basic block.
#[derive(Debug, Clone)]
pub struct BlockData {
    pub stmts: Vec<Statement>,
    pub term: Terminator,
    pub term_span: Span,
}

/// What a body was lowered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Fn,
    Closure,
    GoBlock,
}

/// A lowered function, closure, or `spawn` block.
#[derive(Debug, Clone)]
pub struct Body {
    pub name: String,
    pub kind: BodyKind,
    pub locals: Vec<LocalDecl>,
    pub blocks: Vec<BlockData>,
    /// Locals `_1..=_arg_count` are the parameters (`self` first, if any).
    pub arg_count: usize,
    /// Parameter indices (0-based, `self` first) that the return value may borrow
    /// from, according to the signature. `None` means unchecked (closures).
    pub return_flows: Option<Vec<usize>>,
    pub span: Span,
    /// Errors found during lowering (e.g. a deferred call using a variable
    /// that is dropped before the function returns).
    pub errors: Vec<(String, Span)>,
    /// Constructs the code generator does not support yet.
    pub unsupported: Vec<(String, Span)>,
    /// For closures and `spawn` blocks: the upvar locals and how each is captured,
    /// in the order of the capture operands.
    pub captures: Vec<(Local, CaptureMode)>,
    /// Heads of `for` loops over a range whose trip count is a small
    /// compile-time bound ([`BOUNDED_TRIPS`]). Their back-edges need no
    /// preemption check, so the optimizer can fully unroll them.
    pub bounded_loops: Vec<BlockId>,
}

/// The largest trip count for which a loop counts as bounded.
pub const BOUNDED_TRIPS: i128 = 4096;

impl Body {
    pub fn local(&self, l: Local) -> &LocalDecl {
        &self.locals[l.0 as usize]
    }

    pub fn block(&self, b: BlockId) -> &BlockData {
        &self.blocks[b.0 as usize]
    }

    /// Human-readable name of a place for diagnostics, like `p.name` or `*r`.
    pub fn describe(&self, p: &Place) -> String {
        let decl = self.local(p.local);
        let mut s = match &decl.name {
            Some(n) => n.clone(),
            None => return "value".into(),
        };
        for pr in &p.proj {
            match pr {
                Proj::Deref(_) => s = format!("*{s}"),
                Proj::Field(f) => {
                    let f = f.rsplit('.').next().unwrap_or(f);
                    s = format!("{s}.{f}");
                }
                Proj::Index(_) => s = format!("{s}[..]"),
            }
        }
        s
    }
}
