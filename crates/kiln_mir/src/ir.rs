//! The MIR data structures.
//!
//! A [`Body`] is a control-flow graph of [`BlockData`]s. Each block holds
//! [`Statement`]s and ends in a [`Terminator`]. Values live in [`Local`]s and
//! are addressed with [`Place`]s (a local plus field/deref/index projections).
//! Operands say whether a place is *copied* (read) or *moved*.
//!
//! The IR is designed for borrow checking: it records every read, move,
//! borrow, write, and end of storage, together with how borrows flow from
//! call arguments into results ([`Rvalue::Call::flows`]).

#![allow(missing_docs)] // field names are self-describing; see the module docs

use kiln_lexer::Span;
use kiln_resolve::LocalId;
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
    /// A variable captured by a closure or `go` block (only in that body).
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

/// One step of a place projection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proj {
    /// `*p`
    Deref(DerefKindKey),
    /// `p.name`, `p.0`, or `p.Variant.0` (enum fields are prefixed with the variant name).
    Field(String),
    /// `p[i]` (any index).
    Index,
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

    /// Whether one place contains the other (same local, one projection list is a prefix of the other).
    pub fn overlaps(&self, other: &Place) -> bool {
        if self.local != other.local {
            return false;
        }
        self.proj
            .iter()
            .zip(&other.proj)
            .all(|(a, b)| a == b || *a == Proj::Index || *b == Proj::Index)
    }
}

/// A value used by an rvalue.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// Read the place (for `Copy` types, or a non-consuming read).
    Copy(Place),
    /// Move out of the place.
    Move(Place),
    /// A constant (literals, function items, unit variants).
    Const,
}

impl Operand {
    /// The place read or moved, if any.
    pub fn place(&self) -> Option<&Place> {
        match self {
            Operand::Copy(p) | Operand::Move(p) => Some(p),
            Operand::Const => None,
        }
    }
}

/// The right-hand side of an assignment.
#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    /// The operand's value.
    Use(Operand),
    /// `&place` / `&mut place`.
    Ref { mutable: bool, place: Place },
    /// A struct, tuple, array, enum variant, range, or closure value built from
    /// operands. The result holds any borrows the operands hold.
    Aggregate(Vec<Operand>),
    /// A call. The result holds the borrows held by `args[i]` for each `i` in `flows`
    /// (computed from the callee's signature and lifetime elision).
    Call {
        args: Vec<Operand>,
        flows: Vec<usize>,
    },
    /// Arithmetic, comparisons, casts, channel operations: the result holds no borrows.
    Pure(Vec<Operand>),
}

impl Rvalue {
    /// All operands.
    pub fn operands(&self) -> Vec<&Operand> {
        match self {
            Rvalue::Use(o) => vec![o],
            Rvalue::Ref { .. } => vec![],
            Rvalue::Aggregate(os) | Rvalue::Call { args: os, .. } | Rvalue::Pure(os) => {
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

/// Which kind of task a [`StmtKind::Spawn`] starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    /// `go f(args)`: the operands are the moved arguments.
    GoCall,
    /// `go { ... }` / `go move { ... }`: the operands are the captures.
    GoBlock { is_move: bool },
    /// `s.spawn(closure)` inside `scope(...)`: may borrow locals.
    Scoped,
}

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `place = rvalue`
    Assign(Place, Rvalue),
    /// A non-consuming read of a place (match scrutinees, comparisons, printing).
    Read(Place),
    /// Evaluate an rvalue and discard the result.
    Eval(Rvalue),
    /// The local's scope ends: it is dropped and its storage invalidated.
    StorageDead(Local),
    /// Start a task.
    Spawn {
        kind: SpawnKind,
        operands: Vec<Operand>,
        captures: Vec<Capture>,
    },
}

impl PartialEq for Capture {
    fn eq(&self, o: &Self) -> bool {
        self.name == o.name && self.mode == o.mode && self.span == o.span
    }
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
    /// Branch to any of the targets (conditions are evaluated by preceding statements).
    Branch(Vec<BlockId>),
    Return,
    Unreachable,
}

impl Terminator {
    /// Successor blocks.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::Branch(bs) => bs.clone(),
            Terminator::Return | Terminator::Unreachable => vec![],
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

/// A lowered function, closure, or `go` block.
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
}

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
                Proj::Index => s = format!("{s}[..]"),
            }
        }
        s
    }
}
