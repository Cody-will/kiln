//! Lowering from the typed AST to MIR.

use crate::ir::*;
use crate::sig;
use kiln_lexer::Span;
use kiln_parser::ast::{
    self, Block, Expr, ExprKind, GoTarget, Pattern, PatternKind, SelectOp, StmtKind as AstStmt,
};
use kiln_resolve::{Builtin, DefKind, LocalId, Res, Resolved};
use kiln_typeck::ty::Ty;
use kiln_typeck::{FnResults, Typed};
use std::collections::HashMap;
use std::rc::Rc;

struct LoopCx {
    label: Option<String>,
    brk: BlockId,
    cont: BlockId,
    result: Option<Place>,
}

/// Builds one [`Body`].
pub(crate) struct Lower<'t, 'r, 'a> {
    typed: &'t mut Typed<'r, 'a>,
    r: &'r Resolved<'a>,
    res: Rc<FnResults>,
    body: Body,
    cur: BlockId,
    exit: BlockId,
    map: HashMap<LocalId, Local>,
    scopes: Vec<Vec<Local>>,
    loops: Vec<LoopCx>,
    /// Deferred calls and whether each is inside a loop.
    defers: Vec<(Expr, bool)>,
    /// For closures and `go` blocks: variables captured from the parent.
    is_nested: bool,
    upvars: Vec<(LocalId, Local)>,
    /// Bodies of closures and `go` blocks lowered while lowering this one.
    pub(crate) extra: Vec<Body>,
    last_captures: Option<Vec<Capture>>,
    /// Locals holding closures that move a captured variable out (callable once).
    once: std::collections::HashSet<Local>,
    /// Locals whose scope has ended.
    dead: std::collections::HashSet<Local>,
    /// Set while lowering deferred calls in the exit block.
    in_defer: bool,
    /// Whether the closure just lowered moves a captured variable.
    last_consumes: bool,
}

fn dummy_span() -> Span {
    Span::new(0, 0)
}

impl<'t, 'r, 'a> Lower<'t, 'r, 'a> {
    fn new(
        typed: &'t mut Typed<'r, 'a>,
        res: Rc<FnResults>,
        name: String,
        kind: BodyKind,
        span: Span,
    ) -> Self {
        let r = typed.resolved();
        let body = Body {
            name,
            kind,
            locals: vec![],
            blocks: vec![],
            arg_count: 0,
            return_flows: None,
            errors: vec![],
            span,
        };
        let mut l = Lower {
            typed,
            r,
            res,
            body,
            cur: BlockId(0),
            exit: BlockId(1),
            map: HashMap::new(),
            scopes: vec![vec![]],
            loops: vec![],
            defers: vec![],
            is_nested: kind != BodyKind::Fn,
            upvars: vec![],
            extra: vec![],
            last_captures: None,
            once: Default::default(),
            dead: Default::default(),
            in_defer: false,
            last_consumes: false,
        };
        l.new_block(); // entry
        l.new_block(); // exit
        l
    }

    // ----- building blocks ---------------------------------------------------------

    fn new_block(&mut self) -> BlockId {
        self.body.blocks.push(BlockData {
            stmts: vec![],
            term: Terminator::Unreachable,
            term_span: dummy_span(),
        });
        BlockId(self.body.blocks.len() as u32 - 1)
    }

    fn push(&mut self, kind: StmtKind, span: Span) {
        let b = self.cur.0 as usize;
        self.body.blocks[b].stmts.push(Statement { kind, span });
    }

    fn terminate(&mut self, term: Terminator, span: Span) {
        let b = self.cur.0 as usize;
        self.body.blocks[b].term = term;
        self.body.blocks[b].term_span = span;
    }

    fn goto(&mut self, target: BlockId, span: Span) {
        self.terminate(Terminator::Goto(target), span);
    }

    fn new_local(
        &mut self,
        ty: Ty,
        name: Option<String>,
        span: Span,
        kind: LocalKind,
        mutable: bool,
        source: Option<LocalId>,
    ) -> Local {
        self.body.locals.push(LocalDecl {
            ty,
            name,
            span,
            kind,
            mutable,
            source,
        });
        Local(self.body.locals.len() as u32 - 1)
    }

    fn temp(&mut self, ty: Ty, span: Span) -> Local {
        let l = self.new_local(ty, None, span, LocalKind::Temp, true, None);
        self.scopes.last_mut().unwrap().push(l);
        l
    }

    fn declare(&mut self, id: LocalId) -> Local {
        let decl = self.r.local(id).clone();
        let ty = self.res.locals.get(&id).cloned().unwrap_or(Ty::Unknown);
        let l = self.new_local(
            ty,
            Some(decl.name.clone()),
            decl.span,
            LocalKind::User,
            decl.mutable,
            Some(id),
        );
        self.scopes.last_mut().unwrap().push(l);
        self.map.insert(id, l);
        l
    }

    /// The MIR local for a resolver local, creating an upvar in nested bodies.
    fn lookup(&mut self, id: LocalId, span: Span) -> Local {
        if let Some(l) = self.map.get(&id) {
            return *l;
        }
        let decl = self.r.local(id).clone();
        let ty = self.res.locals.get(&id).cloned().unwrap_or(Ty::Unknown);
        let kind = if self.is_nested {
            LocalKind::Upvar
        } else {
            LocalKind::User
        };
        let l = self.new_local(
            ty,
            Some(decl.name.clone()),
            span,
            kind,
            decl.mutable,
            Some(id),
        );
        if self.is_nested {
            self.upvars.push((id, l));
        }
        self.map.insert(id, l);
        l
    }

    fn ty(&self, e: &Expr) -> Ty {
        self.res.tys.get(&e.span).cloned().unwrap_or(Ty::Unknown)
    }

    fn is_copy(&mut self, t: &Ty) -> bool {
        let bounds = self.res.bounds.clone();
        self.typed.is_copy(t, &bounds)
    }

    fn operand_of(&mut self, p: Place, ty: &Ty) -> Operand {
        if p.proj.is_empty() && self.once.contains(&p.local) {
            return Operand::Move(p);
        }
        if self.is_copy(ty) {
            Operand::Copy(p)
        } else {
            Operand::Move(p)
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(vec![]);
    }

    fn pop_scope(&mut self, end: Span) {
        let locals = self.scopes.pop().unwrap();
        self.dead.extend(locals.iter().copied());
        for l in locals.into_iter().rev() {
            self.push(StmtKind::StorageDead(l), end);
        }
    }

    // ----- entry points --------------------------------------------------------------

    fn finish(mut self, end: Span) -> (Body, Vec<(LocalId, Local)>, Vec<Body>) {
        self.goto(self.exit, end);
        let defers = std::mem::take(&mut self.defers);
        let n = defers.len();
        let targets: Vec<BlockId> = (0..n).map(|_| self.new_block()).collect();
        // `after[i]`: dispatch used after running once-only defer `i` (only defers < i remain).
        let after: Vec<BlockId> = (0..n).map(|_| self.new_block()).collect();
        let done = self.new_block();
        self.in_defer = true;
        for (i, (d, in_loop)) in defers.iter().enumerate() {
            self.cur = targets[i];
            self.expr_into(d, None);
            let next = if *in_loop { self.exit } else { after[i] };
            self.goto(next, d.span);
            self.cur = after[i];
            let t = (0..n).map(|j| (j < i).then_some(targets[j])).collect();
            self.terminate(Terminator::DeferDispatch { targets: t, done }, end);
        }
        self.in_defer = false;
        self.cur = self.exit;
        let all = targets.iter().map(|t| Some(*t)).collect();
        self.terminate(Terminator::DeferDispatch { targets: all, done }, end);
        self.cur = done;
        // Remaining locals die at function exit, in reverse declaration order.
        while self.scopes.len() > 1 {
            self.pop_scope(end);
        }
        let outer = self.scopes.pop().unwrap();
        let mut all: Vec<Local> = outer;
        for (i, d) in self.body.locals.iter().enumerate() {
            if d.kind == LocalKind::Arg && !all.contains(&Local(i as u32)) {
                all.insert(0, Local(i as u32));
            }
        }
        for l in all.into_iter().rev() {
            self.push(StmtKind::StorageDead(l), end);
        }
        self.terminate(Terminator::Return, end);
        (self.body, self.upvars, self.extra)
    }

    fn end_span(b: &Block) -> Span {
        Span::new(b.span.end.saturating_sub(1), b.span.end)
    }

    // ----- statements ------------------------------------------------------------------

    /// Lowers a function's (or closure's) outermost block in the function scope,
    /// so its variables live until after deferred calls run.
    fn body_into(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn block_into(&mut self, b: &Block, dest: Option<Place>) {
        self.push_scope();
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            match (&s.kind, &dest) {
                (AstStmt::Expr(e), Some(d)) if i + 1 == n => self.expr_into(e, Some(d.clone())),
                _ => self.stmt(s),
            }
        }
        self.pop_scope(Self::end_span(b));
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            AstStmt::Let { pat, init, .. } => match init {
                Some(init) => {
                    if let PatternKind::Ident {
                        name, sub: None, ..
                    } = &pat.kind
                    {
                        if let Some(&id) = self.r.bindings.get(&name.span) {
                            let l = self.declare(id);
                            self.expr_into(init, Some(Place::local(l)));
                            return;
                        }
                    }
                    let ty = self.ty(init);
                    let p = self.place_or_temp(init);
                    self.bind_pattern(pat, p, &ty, None);
                }
                None => self.declare_pattern(pat),
            },
            AstStmt::ShortDecl { names, tuple, init } => {
                if !tuple {
                    match names[0]
                        .name
                        .as_ref()
                        .and_then(|n| self.r.bindings.get(&n.span))
                    {
                        Some(&id) => {
                            let l = self.declare(id);
                            self.expr_into(init, Some(Place::local(l)));
                        }
                        None => self.expr_into(init, None),
                    }
                    return;
                }
                let ty = self.ty(init);
                let t = self.temp(ty.clone(), init.span);
                self.expr_into(init, Some(Place::local(t)));
                let elems = match &ty {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => vec![Ty::Unknown; names.len()],
                };
                for (i, n) in names.iter().enumerate() {
                    let Some(name) = &n.name else { continue };
                    let Some(&id) = self.r.bindings.get(&name.span) else {
                        continue;
                    };
                    let l = self.declare(id);
                    let et = elems.get(i).cloned().unwrap_or(Ty::Unknown);
                    let op =
                        self.operand_of(Place::local(t).project(Proj::Field(i.to_string())), &et);
                    self.push(
                        StmtKind::Assign(Place::local(l), Rvalue::Use(op)),
                        name.span,
                    );
                }
            }
            AstStmt::Assign { target, op, value } => {
                let v = self.expr(value);
                let Some(tp) = self.place(target) else {
                    self.push(StmtKind::Eval(Rvalue::Use(v)), s.span);
                    return;
                };
                let rv = match op {
                    None => Rvalue::Use(v),
                    Some(_) => Rvalue::Pure(vec![Operand::Copy(tp.clone()), v]),
                };
                self.push(StmtKind::Assign(tp, rv), s.span);
            }
            AstStmt::Send { chan, value } => {
                let c = self.read(chan);
                let v = self.expr(value);
                self.push(StmtKind::Eval(Rvalue::Pure(vec![c, v])), s.span);
            }
            AstStmt::Defer(e) => {
                self.push(StmtKind::DeferPush(self.defers.len() as u32), s.span);
                let in_loop = !self.loops.is_empty();
                self.defers.push((e.clone(), in_loop));
            }
            AstStmt::Go(GoTarget::Call(e)) => {
                let operands = match &e.kind {
                    ExprKind::Call(f, args) => self.call_operands(f, args).0,
                    ExprKind::MethodCall { receiver, args, .. } => {
                        self.method_operands(e, receiver, args).0
                    }
                    _ => vec![self.expr(e)],
                };
                self.push(
                    StmtKind::Spawn {
                        kind: SpawnKind::GoCall,
                        operands,
                        captures: vec![],
                    },
                    e.span,
                );
            }
            AstStmt::Go(GoTarget::Block { is_move, body }) => {
                let (operands, captures) = self.nested(
                    &[],
                    None,
                    NestedBody::Block(body),
                    *is_move,
                    BodyKind::GoBlock,
                    s.span,
                    Ty::Unit,
                );
                self.push(
                    StmtKind::Spawn {
                        kind: SpawnKind::GoBlock { is_move: *is_move },
                        operands,
                        captures,
                    },
                    s.span,
                );
            }
            AstStmt::Item(_) => {}
            AstStmt::Expr(e) => self.expr_into(e, None),
        }
    }

    fn declare_pattern(&mut self, p: &Pattern) {
        for id in pattern_bindings(self.r, p) {
            self.declare(id);
        }
    }

    // ----- patterns --------------------------------------------------------------------

    /// Binds the variables in `pat` to parts of `place` (of type `ty`). `mode` is
    /// the default binding mode when matching through a reference.
    fn bind_pattern(&mut self, pat: &Pattern, place: Place, ty: &Ty, mode: Option<bool>) {
        let destructures = matches!(
            pat.kind,
            PatternKind::Tuple(_)
                | PatternKind::TupleStruct(..)
                | PatternKind::Struct { .. }
                | PatternKind::Slice(_)
        );
        if destructures {
            if let Ty::Ref(m, inner) = ty {
                let mode = Some(mode.unwrap_or(*m) && *m);
                let p = place.project(Proj::Deref(DerefKind::Ref(*m).into()));
                return self.bind_pattern(pat, p, inner, mode);
            }
        }
        match &pat.kind {
            PatternKind::Ident { name, sub, .. } => {
                if let Some(&id) = self.r.bindings.get(&name.span) {
                    let l = self.declare(id);
                    let rv = match mode {
                        Some(m) => Rvalue::Ref {
                            mutable: m,
                            place: place.clone(),
                        },
                        None => Rvalue::Use(self.operand_of(place.clone(), ty)),
                    };
                    self.push(StmtKind::Assign(Place::local(l), rv), name.span);
                }
                if let Some(s) = sub {
                    self.bind_pattern(s, place, ty, mode);
                }
            }
            PatternKind::Tuple(ps) => {
                let ts = match ty {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => vec![],
                };
                let rest = ps.iter().position(|p| matches!(p.kind, PatternKind::Rest));
                for (i, p) in ps.iter().enumerate() {
                    let idx = match rest {
                        Some(r) if i > r => ts.len().saturating_sub(ps.len() - i),
                        _ => i,
                    };
                    let et = ts.get(idx).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pattern(p, place.project(Proj::Field(idx.to_string())), &et, mode);
                }
            }
            PatternKind::TupleStruct(path, ps) => {
                let (prefix, fields) = self.ctor_fields(path.span, ty);
                for (i, p) in ps.iter().enumerate() {
                    let (n, ft) = fields
                        .get(i)
                        .cloned()
                        .unwrap_or((i.to_string(), Ty::Unknown));
                    self.bind_pattern(
                        p,
                        place.project(Proj::Field(format!("{prefix}{n}"))),
                        &ft,
                        mode,
                    );
                }
            }
            PatternKind::Struct {
                path, fields: fps, ..
            } => {
                let (prefix, fields) = self.ctor_fields(path.span, ty);
                for fp in fps {
                    let ft = fields
                        .iter()
                        .find(|(n, _)| *n == fp.name.name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Ty::Unknown);
                    let p = place.project(Proj::Field(format!("{prefix}{}", fp.name.name)));
                    self.bind_pattern(&fp.pat, p, &ft, mode);
                }
            }
            PatternKind::Slice(ps) => {
                let et = match ty {
                    Ty::Array(t) | Ty::Slice(t) => (**t).clone(),
                    Ty::Adt(_, args) if !args.is_empty() => args[0].clone(),
                    _ => Ty::Unknown,
                };
                for p in ps {
                    self.bind_pattern(p, place.project(Proj::Index), &et, mode);
                }
            }
            PatternKind::Ref { mutable, inner } => {
                let it = match ty {
                    Ty::Ref(_, t) => (**t).clone(),
                    _ => Ty::Unknown,
                };
                self.bind_pattern(
                    inner,
                    place.project(Proj::Deref(DerefKind::Ref(*mutable).into())),
                    &it,
                    None,
                );
            }
            PatternKind::Or(alts) => {
                if let Some(first) = alts.first() {
                    self.bind_pattern(first, place, ty, mode);
                }
            }
            PatternKind::Wild
            | PatternKind::Rest
            | PatternKind::Lit(_)
            | PatternKind::Range { .. }
            | PatternKind::Path(_) => {}
        }
    }

    /// Field-name prefix and field types for a constructor pattern path.
    fn ctor_fields(&self, path_span: Span, ty: &Ty) -> (String, Vec<(String, Ty)>) {
        let variant = match self.r.path(path_span) {
            Res::Def(d) if matches!(self.r.def(d).kind, DefKind::Variant { .. }) => Some(d),
            Res::Assoc { name, .. } => match ty {
                Ty::Adt(e, _) => self
                    .r
                    .variants(*e)
                    .into_iter()
                    .find(|v| self.r.def(*v).name == name),
                _ => None,
            },
            _ => None,
        };
        let prefix = variant
            .map(|v| format!("{}.", self.r.def(v).name))
            .unwrap_or_default();
        (prefix, self.typed.adt_fields(ty, variant))
    }

    // ----- places ----------------------------------------------------------------------

    /// Adds deref projections for each reference/box layer of `ty`, returning the inner type.
    fn autoderef(&self, mut p: Place, mut ty: Ty, max: usize) -> (Place, Ty) {
        for _ in 0..max {
            match ty.clone() {
                Ty::Ref(m, inner) => {
                    p = p.project(Proj::Deref(DerefKind::Ref(m).into()));
                    ty = *inner;
                }
                Ty::Ptr(_, inner) => {
                    p = p.project(Proj::Deref(DerefKind::Ptr.into()));
                    ty = *inner;
                }
                Ty::Adt(d, args) if self.r.prelude_type("Box") == Some(d) => {
                    p = p.project(Proj::Deref(DerefKind::Box.into()));
                    ty = args[0].clone();
                }
                _ => break,
            }
        }
        (p, ty)
    }

    /// The place an expression denotes, if it is a place expression.
    fn place(&mut self, e: &Expr) -> Option<Place> {
        match &e.kind {
            ExprKind::Paren(x) => self.place(x),
            ExprKind::Path(p) => match self.r.path(p.span) {
                Res::Local(id) => {
                    let l = self.lookup(id, e.span);
                    if self.in_defer && self.dead.contains(&l) {
                        let name = self.r.local(id).name.clone();
                        self.body.errors.push((
                            format!("`{name}` is dropped before this deferred call runs"),
                            e.span,
                        ));
                    }
                    Some(Place::local(l))
                }
                _ => None,
            },
            ExprKind::Field(base, name) => {
                let bt = self.ty(base);
                let bp = self.place_or_temp(base);
                let (p, _) = self.autoderef(bp, bt, 8);
                Some(p.project(Proj::Field(name.name.clone())))
            }
            ExprKind::TupleIndex(base, i) => {
                let bt = self.ty(base);
                let bp = self.place_or_temp(base);
                let (p, _) = self.autoderef(bp, bt, 8);
                Some(p.project(Proj::Field(i.to_string())))
            }
            ExprKind::Index(base, idx) => {
                let i = self.expr(idx);
                self.push(StmtKind::Eval(Rvalue::Pure(vec![i])), idx.span);
                let bt = self.ty(base);
                let bp = self.place_or_temp(base);
                let (p, _) = self.autoderef(bp, bt, 8);
                Some(p.project(Proj::Index))
            }
            ExprKind::Unary(ast::UnOp::Deref, x) => {
                let xt = self.ty(x);
                let xp = self.place_or_temp(x);
                let (p, _) = self.autoderef(xp, xt, 1);
                Some(p)
            }
            _ => None,
        }
    }

    fn place_or_temp(&mut self, e: &Expr) -> Place {
        if let Some(p) = self.place(e) {
            return p;
        }
        let t = self.temp(self.ty(e), e.span);
        self.expr_into(e, Some(Place::local(t)));
        Place::local(t)
    }

    /// Evaluates `e` without consuming it (a read of a place, or a temporary).
    fn read(&mut self, e: &Expr) -> Operand {
        match self.place(e) {
            Some(p) => Operand::Copy(p),
            None => self.expr(e),
        }
    }

    // ----- expressions -----------------------------------------------------------------

    fn temp_of(&mut self, ty: Ty, rv: Rvalue, span: Span) -> Operand {
        let t = self.temp(ty.clone(), span);
        self.push(StmtKind::Assign(Place::local(t), rv), span);
        self.operand_of(Place::local(t), &ty)
    }

    /// Lowers `e` to an operand.
    fn expr(&mut self, e: &Expr) -> Operand {
        let ty = self.ty(e);
        match &e.kind {
            ExprKind::Paren(x) => self.expr(x),
            ExprKind::Path(_)
            | ExprKind::Field(..)
            | ExprKind::TupleIndex(..)
            | ExprKind::Index(..)
            | ExprKind::Unary(ast::UnOp::Deref, _) => match self.place(e) {
                Some(p) => self.operand_of(p, &ty),
                None => Operand::Const,
            },
            ExprKind::Lit(_) | ExprKind::Error => Operand::Const,
            ExprKind::Interp(parts) => {
                for part in parts {
                    if let ast::InterpPart::Expr(x) = part {
                        let op = self.read(x);
                        self.push(StmtKind::Eval(Rvalue::Pure(vec![op])), x.span);
                    }
                }
                Operand::Const
            }
            ExprKind::Unary(op, x) => match op {
                ast::UnOp::Ref | ast::UnOp::RefMut => {
                    let p = self.place_or_temp(x);
                    let rv = Rvalue::Ref {
                        mutable: *op == ast::UnOp::RefMut,
                        place: p,
                    };
                    self.temp_of(ty, rv, e.span)
                }
                ast::UnOp::Recv => {
                    let o = self.read(x);
                    self.temp_of(ty, Rvalue::Pure(vec![o]), e.span)
                }
                _ => {
                    let o = self.expr(x);
                    self.temp_of(ty, Rvalue::Pure(vec![o]), e.span)
                }
            },
            ExprKind::Binary(op, l, r) => {
                use ast::BinOp::*;
                let (a, b) = if matches!(op, Eq | Ne | Lt | Le | Gt | Ge) {
                    (self.read(l), self.read(r))
                } else {
                    (self.expr(l), self.expr(r))
                };
                self.temp_of(ty, Rvalue::Pure(vec![a, b]), e.span)
            }
            ExprKind::Cast(x, _) => {
                let o = self.expr(x);
                self.temp_of(ty, Rvalue::Pure(vec![o]), e.span)
            }
            ExprKind::Call(f, args) => self.call(e, f, args),
            ExprKind::MethodCall { receiver, args, .. } => {
                let (args, flows) = self.method_operands(e, receiver, args);
                self.finish_call(e, args, flows)
            }
            ExprKind::Try(x) => {
                let xt = self.ty(x);
                let inner = self.expr(x);
                let t = self.temp(xt.clone(), x.span);
                self.push(
                    StmtKind::Assign(Place::local(t), Rvalue::Use(inner)),
                    x.span,
                );
                let ok = self.new_block();
                let err = self.new_block();
                self.terminate(Terminator::Branch(vec![ok, err]), e.span);
                self.cur = err;
                let rv = Rvalue::Call {
                    args: vec![Operand::Copy(Place::local(t))],
                    flows: vec![0],
                };
                self.push(StmtKind::Assign(Place::local(Local(0)), rv), e.span);
                self.goto(self.exit, e.span);
                self.cur = ok;
                let rv = Rvalue::Call {
                    args: vec![self.operand_of(Place::local(t), &xt)],
                    flows: vec![0],
                };
                self.temp_of(ty, rv, e.span)
            }
            ExprKind::Range { start, end, .. } => {
                let ops = [start, end]
                    .into_iter()
                    .flatten()
                    .map(|x| self.expr(x))
                    .collect();
                self.temp_of(ty, Rvalue::Aggregate(ops), e.span)
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => {
                if xs.is_empty() {
                    return Operand::Const;
                }
                let ops = xs.iter().map(|x| self.expr(x)).collect();
                self.temp_of(ty, Rvalue::Aggregate(ops), e.span)
            }
            ExprKind::ArrayRepeat(x, n) => {
                let a = self.expr(x);
                let b = self.expr(n);
                self.temp_of(ty, Rvalue::Aggregate(vec![a, b]), e.span)
            }
            ExprKind::StructLit { fields, base, .. } => {
                let mut ops: Vec<Operand> = fields.iter().map(|f| self.expr(&f.value)).collect();
                if let Some(b) = base {
                    ops.push(self.expr(b));
                }
                self.temp_of(ty, Rvalue::Aggregate(ops), e.span)
            }
            ExprKind::Closure {
                is_move,
                params,
                body,
                ..
            } => {
                let ret = match &ty {
                    Ty::Fn(_, r) => (**r).clone(),
                    _ => Ty::Unknown,
                };
                let (ops, caps) = self.nested(
                    params,
                    Some(&ty),
                    NestedBody::Expr(body),
                    *is_move,
                    BodyKind::Closure,
                    e.span,
                    ret,
                );
                let consumes = std::mem::take(&mut self.last_consumes);
                self.last_captures = Some(caps);
                let t = self.temp(ty.clone(), e.span);
                self.push(
                    StmtKind::Assign(Place::local(t), Rvalue::Aggregate(ops)),
                    e.span,
                );
                if consumes {
                    self.once.insert(t);
                }
                self.operand_of(Place::local(t), &ty)
            }
            _ => {
                if matches!(ty, Ty::Unit | Ty::Never | Ty::Unknown) {
                    self.expr_into(e, None);
                    Operand::Const
                } else {
                    let t = self.temp(ty.clone(), e.span);
                    self.expr_into(e, Some(Place::local(t)));
                    self.operand_of(Place::local(t), &ty)
                }
            }
        }
    }

    /// A call argument. Passing a `&mut` place reborrows it (`&mut *r`) instead
    /// of moving the reference, so `r` stays usable after the call.
    fn arg(&mut self, a: &Expr) -> Operand {
        let ty = self.ty(a);
        if let Ty::Ref(true, _) = ty {
            if let Some(p) = self.place(a) {
                let inner = p.project(Proj::Deref(DerefKind::Ref(true).into()));
                return self.temp_of(
                    ty,
                    Rvalue::Ref {
                        mutable: true,
                        place: inner,
                    },
                    a.span,
                );
            }
        }
        self.expr(a)
    }

    fn is_print_or_builtin(&self, f: &Expr) -> Option<&'static str> {
        let ExprKind::Path(p) = &f.kind else {
            return None;
        };
        match self.r.path(p.span) {
            Res::Builtin(Builtin::Chan) => Some("chan"),
            Res::Builtin(Builtin::Drop) => Some("drop"),
            Res::Def(d) => {
                let def = self.r.def(d);
                (def.is_prelude
                    && matches!(
                        def.name.as_str(),
                        "print" | "println" | "eprint" | "eprintln"
                    ))
                .then_some("print")
            }
            _ => None,
        }
    }

    /// Lowers callee and arguments; returns the operands and borrow flows.
    fn call_operands(&mut self, f: &Expr, args: &[Expr]) -> (Vec<Operand>, Vec<usize>) {
        match self.is_print_or_builtin(f) {
            Some("print") => return (args.iter().map(|a| self.read(a)).collect(), vec![]),
            Some(_) => return (args.iter().map(|a| self.expr(a)).collect(), vec![]),
            None => {}
        }
        let callee = self.res.callees.get(&f.span).copied();
        let is_item = matches!(&f.kind, ExprKind::Path(p) if matches!(self.r.path(p.span), Res::Def(_) | Res::Assoc { .. }));
        let mut ops = Vec::new();
        if !is_item {
            let callee = match self.place(f) {
                Some(p) if p.proj.is_empty() && self.once.contains(&p.local) => Operand::Move(p),
                Some(p) => Operand::Copy(p),
                None => self.expr(f),
            };
            ops.push(callee);
        }
        let offset = ops.len();
        for a in args {
            let o = self.arg(a);
            ops.push(o);
        }
        let flows = match callee {
            Some(i) => self.fn_flows(i).into_iter().map(|x| x + offset).collect(),
            None => (0..ops.len()).collect(),
        };
        (ops, flows)
    }

    fn fn_flows(&self, i: usize) -> Vec<usize> {
        let f = &self.r.fns[i];
        let og = f.impl_idx.map(|idx| &self.r.impls[idx].decl.generics);
        let outer = og.map(sig::type_param_names).unwrap_or_default();
        sig::decl_flows(f.decl, &outer, og, self.r)
    }

    fn call(&mut self, e: &Expr, f: &Expr, args: &[Expr]) -> Operand {
        let (ops, flows) = self.call_operands(f, args);
        if self.is_print_or_builtin(f) == Some("print") {
            self.push(StmtKind::Eval(Rvalue::Pure(ops)), e.span);
            return Operand::Const;
        }
        self.finish_call(e, ops, flows)
    }

    fn finish_call(&mut self, e: &Expr, args: Vec<Operand>, flows: Vec<usize>) -> Operand {
        let ty = self.ty(e);
        let rv = Rvalue::Call { args, flows };
        if matches!(ty, Ty::Unit | Ty::Never) {
            self.push(StmtKind::Eval(rv), e.span);
            if ty == Ty::Never {
                self.terminate(Terminator::Unreachable, e.span);
                self.cur = self.new_block();
            }
            return Operand::Const;
        }
        self.temp_of(ty, rv, e.span)
    }

    /// Lowers a method call's receiver (with autoderef/autoref) and arguments.
    /// Arguments are evaluated before the receiver is borrowed (two-phase borrows).
    fn method_operands(
        &mut self,
        e: &Expr,
        recv: &Expr,
        args: &[Expr],
    ) -> (Vec<Operand>, Vec<usize>) {
        let info = self.res.methods.get(&e.span).cloned();
        let base = match self.place(recv) {
            Some(p) => p,
            None => {
                let t = self.temp(self.ty(recv), recv.span);
                self.expr_into(recv, Some(Place::local(t)));
                Place::local(t)
            }
        };
        let arg_ops: Vec<Operand> = args.iter().map(|a| self.arg(a)).collect();
        let Some(info) = info else {
            let mut ops = vec![Operand::Copy(base)];
            ops.extend(arg_ops);
            let n = ops.len();
            return (ops, (0..n).collect());
        };
        let (p, _) = self.autoderef(base, self.ty(recv), info.steps);
        let self_op = match &info.self_param {
            Some(ast::SelfParam::Ref { mutable, .. }) => {
                let rt = Ty::Ref(*mutable, Box::new(info.recv_ty.clone()));
                self.temp_of(
                    rt,
                    Rvalue::Ref {
                        mutable: *mutable,
                        place: p,
                    },
                    recv.span,
                )
            }
            Some(ast::SelfParam::Value { .. }) => self.operand_of(p, &info.recv_ty),
            None => Operand::Copy(p),
        };
        let scoped = info.fn_idx.is_some_and(|i| {
            let f = &self.r.fns[i];
            f.is_prelude && f.decl.name.name == "spawn"
        });
        let mut ops = vec![self_op];
        ops.extend(arg_ops);
        let flows = match info.fn_idx {
            Some(i) => self.fn_flows(i),
            None => (0..ops.len()).collect(),
        };
        if scoped {
            let captures = self.last_captures.take().unwrap_or_default();
            self.push(
                StmtKind::Spawn {
                    kind: SpawnKind::Scoped,
                    operands: vec![],
                    captures,
                },
                e.span,
            );
        }
        (ops, flows)
    }

    /// Lowers `e`, storing its value in `dest` (or discarding it).
    fn expr_into(&mut self, e: &Expr, dest: Option<Place>) {
        match &e.kind {
            ExprKind::Paren(x) => self.expr_into(x, dest),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.block_into(b, dest),
            ExprKind::If { cond, then, els } => {
                let c = self.expr(cond);
                self.push(StmtKind::Eval(Rvalue::Pure(vec![c])), cond.span);
                self.branches(e.span, then, None, els.as_deref(), dest);
            }
            ExprKind::IfLet {
                pat,
                expr,
                then,
                els,
            } => {
                let ty = self.ty(expr);
                let p = self.place_or_temp(expr);
                self.push(StmtKind::Read(p.clone()), expr.span);
                self.branches(e.span, then, Some((pat, p, ty)), els.as_deref(), dest);
            }
            ExprKind::Match(scrut, arms) => {
                let ty = self.ty(scrut);
                let sp = self.place_or_temp(scrut);
                self.push(StmtKind::Read(sp.clone()), scrut.span);
                let join = self.new_block();
                let targets: Vec<BlockId> = arms.iter().map(|_| self.new_block()).collect();
                if targets.is_empty() {
                    self.terminate(Terminator::Unreachable, e.span);
                } else {
                    self.terminate(Terminator::Branch(targets.clone()), e.span);
                }
                for (arm, bb) in arms.iter().zip(targets) {
                    self.cur = bb;
                    self.push_scope();
                    self.bind_pattern(&arm.pat, sp.clone(), &ty, None);
                    if let Some(g) = &arm.guard {
                        let o = self.expr(g);
                        self.push(StmtKind::Eval(Rvalue::Pure(vec![o])), g.span);
                    }
                    self.expr_into(&arm.body, dest.clone());
                    self.pop_scope(arm.span);
                    self.goto(join, arm.span);
                }
                self.cur = join;
            }
            ExprKind::Loop { label, body } => {
                let head = self.new_block();
                let after = self.new_block();
                self.goto(head, e.span);
                self.cur = head;
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: dest,
                });
                self.block_into(body, None);
                self.loops.pop();
                self.goto(head, e.span);
                self.cur = after;
            }
            ExprKind::While { label, cond, body } => {
                let head = self.new_block();
                let bodyb = self.new_block();
                let after = self.new_block();
                self.goto(head, e.span);
                self.cur = head;
                let c = self.expr(cond);
                self.push(StmtKind::Eval(Rvalue::Pure(vec![c])), cond.span);
                self.terminate(Terminator::Branch(vec![bodyb, after]), cond.span);
                self.cur = bodyb;
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: None,
                });
                self.block_into(body, None);
                self.loops.pop();
                self.goto(head, e.span);
                self.cur = after;
            }
            ExprKind::WhileLet {
                label,
                pat,
                expr,
                body,
            } => {
                let head = self.new_block();
                let bodyb = self.new_block();
                let after = self.new_block();
                self.goto(head, e.span);
                self.cur = head;
                let ty = self.ty(expr);
                let p = self.place_or_temp(expr);
                self.push(StmtKind::Read(p.clone()), expr.span);
                self.terminate(Terminator::Branch(vec![bodyb, after]), expr.span);
                self.cur = bodyb;
                self.push_scope();
                self.bind_pattern(pat, p, &ty, None);
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: None,
                });
                self.block_into(body, None);
                self.loops.pop();
                self.pop_scope(Self::end_span(body));
                self.goto(head, e.span);
                self.cur = after;
            }
            ExprKind::For {
                label,
                pat,
                iter,
                body,
            } => {
                let it_ty = self.ty(iter);
                let it_op = self.expr(iter);
                let it = self.temp(it_ty.clone(), iter.span);
                self.push(
                    StmtKind::Assign(Place::local(it), Rvalue::Use(it_op)),
                    iter.span,
                );
                let head = self.new_block();
                let bodyb = self.new_block();
                let after = self.new_block();
                self.goto(head, e.span);
                self.cur = head;
                self.terminate(Terminator::Branch(vec![bodyb, after]), iter.span);
                self.cur = bodyb;
                self.push_scope();
                let elem_ty = self.elem_ty(&it_ty);
                let el = self.temp(elem_ty.clone(), pat.span);
                let rv = Rvalue::Call {
                    args: vec![Operand::Copy(Place::local(it))],
                    flows: vec![0],
                };
                self.push(StmtKind::Assign(Place::local(el), rv), pat.span);
                self.bind_pattern(pat, Place::local(el), &elem_ty, None);
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: None,
                });
                self.block_into(body, None);
                self.loops.pop();
                self.pop_scope(Self::end_span(body));
                self.goto(head, e.span);
                self.cur = after;
            }
            ExprKind::Break(label, val) => {
                let idx = match label {
                    Some(l) => self
                        .loops
                        .iter()
                        .rposition(|c| c.label.as_deref() == Some(l.name.as_str())),
                    None => self.loops.len().checked_sub(1),
                };
                let Some(i) = idx else { return };
                if let Some(v) = val {
                    let d = self.loops[i].result.clone();
                    self.expr_into(v, d);
                }
                let brk = self.loops[i].brk;
                self.goto(brk, e.span);
                self.cur = self.new_block();
            }
            ExprKind::Continue(label) => {
                let idx = match label {
                    Some(l) => self
                        .loops
                        .iter()
                        .rposition(|c| c.label.as_deref() == Some(l.name.as_str())),
                    None => self.loops.len().checked_sub(1),
                };
                if let Some(i) = idx {
                    let c = self.loops[i].cont;
                    self.goto(c, e.span);
                    self.cur = self.new_block();
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr_into(v, Some(Place::local(Local(0))));
                }
                self.goto(self.exit, e.span);
                self.cur = self.new_block();
            }
            ExprKind::Select(arms) => {
                let join = self.new_block();
                let targets: Vec<BlockId> = arms.iter().map(|_| self.new_block()).collect();
                self.terminate(Terminator::Branch(targets.clone()), e.span);
                for (arm, bb) in arms.iter().zip(targets) {
                    self.cur = bb;
                    self.push_scope();
                    match &arm.op {
                        SelectOp::Recv { bind, chan } => {
                            let c = self.read(chan);
                            match bind
                                .as_ref()
                                .and_then(|b| b.name.as_ref())
                                .and_then(|n| self.r.bindings.get(&n.span))
                            {
                                Some(&id) => {
                                    let l = self.declare(id);
                                    self.push(
                                        StmtKind::Assign(Place::local(l), Rvalue::Pure(vec![c])),
                                        chan.span,
                                    );
                                }
                                None => self.push(StmtKind::Eval(Rvalue::Pure(vec![c])), chan.span),
                            }
                        }
                        SelectOp::Send { chan, value } => {
                            let c = self.read(chan);
                            let v = self.expr(value);
                            self.push(StmtKind::Eval(Rvalue::Pure(vec![c, v])), arm.span);
                        }
                        SelectOp::Default => {}
                    }
                    self.expr_into(&arm.body, None);
                    self.pop_scope(arm.span);
                    self.goto(join, arm.span);
                }
                self.cur = join;
            }
            _ => {
                let op = self.expr(e);
                match dest {
                    Some(d) => {
                        if let Some(p) = op.place() {
                            if d.proj.is_empty()
                                && p.proj.is_empty()
                                && self.once.contains(&p.local)
                            {
                                self.once.insert(d.local);
                            }
                        }
                        self.push(StmtKind::Assign(d, Rvalue::Use(op)), e.span)
                    }
                    None => {
                        if !matches!(op, Operand::Const) {
                            self.push(StmtKind::Eval(Rvalue::Use(op)), e.span)
                        }
                    }
                }
            }
        }
    }

    /// `if` / `if let` branches.
    fn branches(
        &mut self,
        span: Span,
        then: &Block,
        pat: Option<(&Pattern, Place, Ty)>,
        els: Option<&Expr>,
        dest: Option<Place>,
    ) {
        let tb = self.new_block();
        let eb = self.new_block();
        let join = self.new_block();
        self.terminate(Terminator::Branch(vec![tb, eb]), span);
        self.cur = tb;
        self.push_scope();
        if let Some((p, place, ty)) = pat {
            self.bind_pattern(p, place, &ty, None);
        }
        self.block_into(then, dest.clone());
        self.pop_scope(Self::end_span(then));
        self.goto(join, span);
        self.cur = eb;
        if let Some(e) = els {
            self.expr_into(e, dest);
        }
        self.goto(join, span);
        self.cur = join;
    }

    fn elem_ty(&self, t: &Ty) -> Ty {
        let is = |d: kiln_resolve::DefId, n: &str| self.r.prelude_type(n) == Some(d);
        match t {
            Ty::Adt(d, a) if (is(*d, "Range") || is(*d, "Vec")) && !a.is_empty() => a[0].clone(),
            Ty::Array(x) | Ty::Slice(x) | Ty::ChanRecv(x) => (**x).clone(),
            Ty::Ref(m, inner) => match &**inner {
                Ty::Adt(d, a) if is(*d, "Vec") => Ty::Ref(*m, Box::new(a[0].clone())),
                Ty::Array(x) | Ty::Slice(x) => Ty::Ref(*m, x.clone()),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    // ----- closures and go blocks ---------------------------------------------------------

    /// Lowers a closure or `go` block into its own body and returns the capture
    /// operands (evaluated in this body) and capture descriptions.
    #[allow(clippy::too_many_arguments)]
    fn nested(
        &mut self,
        params: &[ast::ClosureParam],
        fn_ty: Option<&Ty>,
        body: NestedBody<'_>,
        is_move: bool,
        kind: BodyKind,
        span: Span,
        ret: Ty,
    ) -> (Vec<Operand>, Vec<Capture>) {
        let name = format!(
            "{}::{{{}}}",
            self.body.name,
            if kind == BodyKind::GoBlock {
                "go"
            } else {
                "closure"
            }
        );
        let res = self.res.clone();
        let mut inner = Lower::new(&mut *self.typed, res, name, kind, span);
        inner.new_local(ret, None, span, LocalKind::Return, true, None);
        let param_tys = match fn_ty {
            Some(Ty::Fn(ps, _)) => ps.clone(),
            _ => vec![],
        };
        let mut pending = vec![];
        for (i, p) in params.iter().enumerate() {
            let t = param_tys.get(i).cloned().unwrap_or(Ty::Unknown);
            let a = inner.new_local(t.clone(), None, p.pat.span, LocalKind::Arg, false, None);
            pending.push((p, a, t));
        }
        inner.body.arg_count = params.len();
        for (p, a, t) in pending {
            inner.bind_pattern(&p.pat, Place::local(a), &t, None);
        }
        let end = match body {
            NestedBody::Block(b) => {
                inner.body_into(b);
                Self::end_span(b)
            }
            NestedBody::Expr(e) => {
                match &e.kind {
                    ExprKind::Block(b) => inner.body_into(b),
                    _ => inner.expr_into(e, Some(Place::local(Local(0)))),
                }
                Span::new(e.span.end.saturating_sub(1), e.span.end)
            }
        };
        let (nb, upvars, extra) = inner.finish(end);
        let mut ops = vec![];
        let mut caps = vec![];
        self.last_consumes = false;
        for (id, ul) in &upvars {
            let inferred = capture_mode(&nb, *ul);
            if inferred == CaptureMode::Move {
                self.last_consumes = true;
            }
            let mode = if is_move { CaptureMode::Move } else { inferred };
            let decl = nb.local(*ul).clone();
            let pl = Place::local(self.lookup(*id, span));
            let op = match mode {
                CaptureMode::Ref | CaptureMode::MutRef => {
                    let m = mode == CaptureMode::MutRef;
                    let rt = Ty::Ref(m, Box::new(decl.ty.clone()));
                    self.temp_of(
                        rt,
                        Rvalue::Ref {
                            mutable: m,
                            place: pl,
                        },
                        decl.span,
                    )
                }
                CaptureMode::Move => self.operand_of(pl, &decl.ty),
            };
            ops.push(op);
            caps.push(Capture {
                name: decl.name.clone().unwrap_or_default(),
                ty: decl.ty.clone(),
                mode,
                span: decl.span,
            });
        }
        self.extra.push(nb);
        self.extra.extend(extra);
        (ops, caps)
    }
}

enum NestedBody<'e> {
    Block(&'e Block),
    Expr(&'e Expr),
}

/// Infers how a closure body uses upvar `u`: moved, mutated, or only read.
fn capture_mode(b: &Body, u: Local) -> CaptureMode {
    let mut mode = CaptureMode::Ref;
    let roots = |p: &Place| p.local == u && !p.through_ref();
    for bb in &b.blocks {
        for s in &bb.stmts {
            let mut ops: Vec<&Operand> = vec![];
            match &s.kind {
                StmtKind::Assign(p, rv) => {
                    if roots(p) {
                        mode = CaptureMode::MutRef;
                    }
                    if let Rvalue::Ref {
                        mutable: true,
                        place,
                    } = rv
                    {
                        if roots(place) {
                            mode = CaptureMode::MutRef;
                        }
                    }
                    ops = rv.operands();
                }
                StmtKind::Eval(rv) => ops = rv.operands(),
                StmtKind::Spawn { operands, .. } => ops = operands.iter().collect(),
                _ => {}
            }
            for o in ops {
                if let Operand::Move(p) = o {
                    if roots(p) {
                        return CaptureMode::Move;
                    }
                }
            }
        }
    }
    mode
}

/// Resolver ids of the variables a pattern binds.
fn pattern_bindings(r: &Resolved<'_>, p: &Pattern) -> Vec<LocalId> {
    let mut out = vec![];
    fn go(r: &Resolved<'_>, p: &Pattern, out: &mut Vec<LocalId>) {
        match &p.kind {
            PatternKind::Ident { name, sub, .. } => {
                if let Some(id) = r.bindings.get(&name.span) {
                    out.push(*id);
                }
                if let Some(s) = sub {
                    go(r, s, out);
                }
            }
            PatternKind::Tuple(ps) | PatternKind::Slice(ps) | PatternKind::TupleStruct(_, ps) => {
                ps.iter().for_each(|x| go(r, x, out))
            }
            PatternKind::Or(ps) => {
                if let Some(x) = ps.first() {
                    go(r, x, out)
                }
            }
            PatternKind::Struct { fields, .. } => fields.iter().for_each(|f| go(r, &f.pat, out)),
            PatternKind::Ref { inner, .. } => go(r, inner, out),
            _ => {}
        }
    }
    go(r, p, &mut out);
    out
}

/// Lowers function `i` (an index into `Resolved::fns`). Returns the function's
/// body first, followed by the bodies of its closures and `go` blocks.
pub fn lower_fn(typed: &mut Typed<'_, '_>, i: usize) -> Vec<Body> {
    let r = typed.resolved();
    let f = &r.fns[i];
    let Some(res) = typed.fn_results(i).cloned() else {
        return vec![];
    };
    let Some(body_ast) = &f.decl.body else {
        return vec![];
    };
    let res = Rc::new(res);
    let (params, ret) = typed.signature(i);
    let name = match f.impl_idx {
        Some(_) => format!("{}::{}", self_name(typed, i), f.decl.name.name),
        None => f.decl.name.name.clone(),
    };
    let og = f.impl_idx.map(|idx| &r.impls[idx].decl.generics);
    let outer = og.map(sig::type_param_names).unwrap_or_default();
    let flows = sig::decl_flows(f.decl, &outer, og, r);
    let mut l = Lower::new(typed, res.clone(), name, BodyKind::Fn, f.decl.name.span);
    l.body.return_flows = Some(flows);
    l.new_local(ret, None, f.decl.name.span, LocalKind::Return, true, None);
    if let Some(sl) = f.self_local {
        let t = res.locals.get(&sl).cloned().unwrap_or(Ty::Unknown);
        let decl = r.local(sl);
        let a = l.new_local(
            t,
            Some("self".into()),
            decl.span,
            LocalKind::Arg,
            decl.mutable,
            Some(sl),
        );
        l.map.insert(sl, a);
    }
    let mut pending = vec![];
    for (p, t) in f.decl.params.iter().zip(params) {
        match &p.pat.kind {
            PatternKind::Ident {
                name,
                sub: None,
                mutable,
            } if r.bindings.contains_key(&name.span) => {
                let id = r.bindings[&name.span];
                let a = l.new_local(
                    t,
                    Some(name.name.clone()),
                    name.span,
                    LocalKind::Arg,
                    *mutable,
                    Some(id),
                );
                l.map.insert(id, a);
            }
            _ => {
                let a = l.new_local(t.clone(), None, p.span, LocalKind::Arg, false, None);
                pending.push((p, a, t));
            }
        }
    }
    l.body.arg_count = l.body.locals.len() - 1;
    for (p, a, t) in pending {
        l.bind_pattern(&p.pat, Place::local(a), &t, None);
    }
    l.body_into(body_ast);
    let (body, _, extra) = l.finish(Lower::end_span(body_ast));
    let mut out = vec![body];
    out.extend(extra);
    out
}

fn self_name(typed: &Typed<'_, '_>, i: usize) -> String {
    let r = typed.resolved();
    let idx = r.fns[i].impl_idx.unwrap();
    match &r.impls[idx].decl.self_ty.kind {
        ast::TypeKind::Path(p) => p
            .segments
            .last()
            .map(|s| s.ident.name.clone())
            .unwrap_or_default(),
        _ => "impl".into(),
    }
}
