//! Lowering from the typed AST to MIR.

use crate::ir::*;
use crate::sig;
use kiln_lexer::Span;
use kiln_parser::ast::{
    self, BinOp, Block, Expr, ExprKind, GoTarget, Lit, Pattern, PatternKind, SelectOp,
    StmtKind as AstStmt,
};
use kiln_resolve::{Builtin, DefId, DefKind, LocalId, PrimTy, Res, Resolved};
use kiln_typeck::ty::Ty;
use kiln_typeck::{FnResults, Typed};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Field-name prefix, field types, and `(enum, variant index)` of a constructor.
type CtorFields = (String, Vec<(String, Ty)>, Option<(DefId, usize)>);

struct LoopCx {
    label: Option<String>,
    brk: BlockId,
    cont: BlockId,
    result: Option<Place>,
    /// Scopes deeper than this end when leaving the loop body (`break`/`continue`).
    depth: usize,
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
    defers: Vec<Deferred>,
    /// For closures and `go` blocks: variables captured from the parent.
    is_nested: bool,
    upvars: Vec<(LocalId, Local)>,
    /// Bodies of closures and `go` blocks lowered while lowering this one.
    pub(crate) extra: Vec<Body>,
    last_captures: Option<Vec<Capture>>,
    /// Locals holding closures that move a captured variable out (callable once).
    once: HashSet<Local>,
    /// Locals holding closures that may not be used as `fn` values, with why:
    /// they move a capture out, mutate a capture, or capture something not
    /// `Send + Sync`. They can only be called directly.
    local_fns: HashMap<Local, String>,
    /// Set while lowering an expression whose value is bound directly to a
    /// local (or called), where a restricted closure may appear.
    flow_ok: bool,
    /// Lowering the closure argument of `Scope::spawn`.
    spawn_arg: bool,
    /// Locals whose scope has ended.
    dead: HashSet<Local>,
    /// Set while lowering deferred calls in the exit block.
    in_defer: bool,
    /// Whether the closure just lowered moves a captured variable.
    last_consumes: bool,
    /// Set while lowering a place that will be written or mutably borrowed, so
    /// user `Deref` steps call `deref_mut` instead of `deref`.
    want_mut: bool,
}

/// A `defer` statement.
struct Deferred {
    expr: Expr,
    in_loop: bool,
    /// For calls: the tuple local holding the callee and arguments evaluated at
    /// the `defer` statement, and how to make the call from them.
    now: Option<(Local, DeferCall)>,
}

enum DeferCall {
    Call(Callee, Vec<usize>),
    Print(Op),
}

fn dummy_span() -> Span {
    Span::new(0, 0)
}

fn sorted(m: &HashMap<String, Ty>) -> Vec<(String, Ty)> {
    let mut v: Vec<(String, Ty)> = m.iter().map(|(k, t)| (k.clone(), t.clone())).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn usize_ty() -> Ty {
    Ty::Prim(PrimTy::Usize)
}

fn bool_ty() -> Ty {
    Ty::Prim(PrimTy::Bool)
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
            span,
            errors: vec![],
            unsupported: vec![],
            captures: vec![],
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
            once: HashSet::new(),
            local_fns: HashMap::new(),
            flow_ok: false,
            spawn_arg: false,
            dead: HashSet::new(),
            in_defer: false,
            last_consumes: false,
            want_mut: false,
        };
        l.new_block(); // entry
        l.new_block(); // exit
        l
    }

    fn unsupported(&mut self, what: impl Into<String>, span: Span) {
        self.body.unsupported.push((what.into(), span));
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

    fn assign(&mut self, p: Place, rv: Rvalue, span: Span) {
        self.push(StmtKind::Assign(p, rv), span);
    }

    fn terminate(&mut self, term: Terminator, span: Span) {
        let b = self.cur.0 as usize;
        self.body.blocks[b].term = term;
        self.body.blocks[b].term_span = span;
    }

    fn goto(&mut self, target: BlockId, span: Span) {
        self.terminate(Terminator::Goto(target), span);
    }

    /// Ends the current block with `if cond { t } else { f }`.
    fn switch(&mut self, cond: Operand, t: BlockId, f: BlockId, span: Span) {
        self.terminate(Terminator::SwitchBool { cond, t, f }, span);
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

    /// Whether `e` is a closure or a plain name (possibly parenthesized).
    fn direct(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Paren(x) => Self::direct(x),
            ExprKind::Closure { .. } | ExprKind::Path(_) => true,
            _ => false,
        }
    }

    /// Why the closure just lowered (its captures are in `last_captures`)
    /// can't be a `fn` value of type `ty`, if it can't.
    fn closure_restriction(&mut self, ty: &Ty, consumes: bool) -> Option<String> {
        if consumes {
            return Some("moves a captured value out".into());
        }
        if matches!(ty, Ty::Fn(_, _, true)) {
            return None;
        }
        let caps = self.last_captures.clone().unwrap_or_default();
        let bounds = self.res.bounds.clone();
        for c in &caps {
            if c.mode == CaptureMode::MutRef {
                return Some(format!(
                    "mutates the captured variable `{}` (a `fn` must not; declare the parameter `fn mut(..)` instead)",
                    c.name
                ));
            }
        }
        for c in &caps {
            if !self.typed.implements(&c.ty, "Send", &bounds)
                || !self.typed.implements(&c.ty, "Sync", &bounds)
            {
                let t = self.typed.fmt_ty(&c.ty);
                return Some(format!(
                    "captures `{}` of type `{t}`, which is not `Send` and `Sync` (a `fn` value may be shared between tasks)",
                    c.name
                ));
            }
        }
        None
    }

    fn is_copy(&mut self, t: &Ty) -> bool {
        let bounds = self.res.bounds.clone();
        self.typed.is_copy(t, &bounds)
    }

    fn operand_of(&mut self, p: Place, ty: &Ty) -> Operand {
        let ok = std::mem::take(&mut self.flow_ok);
        if p.proj.is_empty() && !ok {
            if let Some(why) = self.local_fns.get(&p.local).cloned() {
                let span = self.body.locals[p.local.0 as usize].span;
                self.body.errors.push((
                    format!("this closure {why}, so it can only be called directly, not passed or stored as a `fn` value"),
                    span,
                ));
            }
        }
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

    /// On an early exit (`break`, `continue`, `return`, `?`): ends the scopes
    /// deeper than `depth` on this path, innermost first. (The function's
    /// outermost scope ends after deferred calls run, in the exit block.)
    fn exit_scopes(&mut self, depth: usize, span: Span) {
        let dying: Vec<Local> = self.scopes[depth.min(self.scopes.len())..]
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev().copied())
            .collect();
        for l in dying {
            self.push(StmtKind::StorageDead(l), span);
        }
    }

    fn lang(&self, d: DefId, name: &str) -> bool {
        self.r.prelude_type(name) == Some(d)
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
        for (i, d) in defers.iter().enumerate() {
            self.cur = targets[i];
            match &d.now {
                Some((rec, call)) => self.run_deferred(*rec, call, d.expr.span),
                None => self.expr_into(&d.expr, None),
            }
            let next = if d.in_loop { self.exit } else { after[i] };
            self.goto(next, d.expr.span);
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
        let mut all: Vec<Local> = self.scopes.pop().unwrap();
        for (i, d) in self.body.locals.iter().enumerate() {
            if d.kind == LocalKind::Arg && !all.contains(&Local(i as u32)) {
                all.insert(0, Local(i as u32));
            }
        }
        for l in all.into_iter().rev() {
            self.push(StmtKind::StorageDead(l), end);
        }
        // A task owns its captures: whatever the body didn't move out is dropped
        // when it finishes.
        if self.body.kind == BodyKind::GoBlock {
            for i in 0..self.body.locals.len() {
                if self.body.locals[i].kind == LocalKind::Upvar {
                    self.push(StmtKind::StorageDead(Local(i as u32)), end);
                }
            }
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
                    let op = self.operand_of(Place::local(t).field(i.to_string()), &et);
                    self.assign(Place::local(l), Rvalue::Use(op), name.span);
                }
            }
            AstStmt::Assign { target, op, value } => {
                let v = self.expr(value);
                let saved = std::mem::replace(&mut self.want_mut, true);
                let tp = self.place(target);
                self.want_mut = saved;
                let Some(tp) = tp else {
                    self.push(StmtKind::Eval(Rvalue::Use(v)), s.span);
                    return;
                };
                let rv = match op {
                    None => Rvalue::Use(v),
                    Some(bop) => Rvalue::Pure(Op::Bin(*bop), vec![Operand::Copy(tp.clone()), v]),
                };
                self.assign(tp, rv, s.span);
            }
            AstStmt::Send { chan, value } => {
                let c = self.read(chan);
                let v = self.expr(value);
                let rv = Rvalue::Call {
                    callee: Callee::Intrinsic("chan_send".into()),
                    args: vec![c, v],
                    flows: vec![],
                };
                self.push(StmtKind::Eval(rv), s.span);
            }
            AstStmt::Defer(e) => {
                let id = self.defers.len() as u32;
                let in_loop = !self.loops.is_empty();
                let now = self.defer_now(e);
                self.push(StmtKind::DeferPush(id, now.as_ref().map(|x| x.0)), s.span);
                self.defers.push(Deferred {
                    expr: e.clone(),
                    in_loop,
                    now,
                });
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

    // ----- patterns ----------------------------------------------------------------------

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
                    self.assign(Place::local(l), rv, name.span);
                }
                if let Some(s) = sub {
                    self.bind_pattern(s, place, ty, mode);
                }
            }
            PatternKind::Tuple(ps) => {
                for (p, idx, et) in self.tuple_elems(ps, ty) {
                    self.bind_pattern(p, place.field(idx.to_string()), &et, mode);
                }
            }
            PatternKind::TupleStruct(path, ps) => {
                let (prefix, fields, _) = self.ctor_fields(path.span, ty);
                for (i, p) in ps.iter().enumerate() {
                    let (n, ft) = fields
                        .get(i)
                        .cloned()
                        .unwrap_or((i.to_string(), Ty::Unknown));
                    self.bind_pattern(p, place.field(format!("{prefix}{n}")), &ft, mode);
                }
            }
            PatternKind::Struct {
                path, fields: fps, ..
            } => {
                let (prefix, fields, _) = self.ctor_fields(path.span, ty);
                for fp in fps {
                    let ft = fields
                        .iter()
                        .find(|(n, _)| *n == fp.name.name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Ty::Unknown);
                    self.bind_pattern(
                        &fp.pat,
                        place.field(format!("{prefix}{}", fp.name.name)),
                        &ft,
                        mode,
                    );
                }
            }
            PatternKind::Slice(ps) => {
                self.unsupported("slice patterns", pat.span);
                let et = match ty {
                    Ty::Array(t, _) | Ty::Slice(t) => (**t).clone(),
                    Ty::Adt(_, args) if !args.is_empty() => args[0].clone(),
                    _ => Ty::Unknown,
                };
                let i = self.temp(usize_ty(), pat.span);
                for p in ps {
                    self.bind_pattern(p, place.project(Proj::Index(i)), &et, mode);
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
                if alts.len() > 1 && !pattern_bindings(self.r, pat).is_empty() {
                    self.unsupported("`|` patterns that bind variables", pat.span);
                }
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

    /// Sub-patterns of a tuple pattern with their element index and type (handling `..`).
    fn tuple_elems<'p>(&self, ps: &'p [Pattern], ty: &Ty) -> Vec<(&'p Pattern, usize, Ty)> {
        let ts = match ty {
            Ty::Tuple(ts) => ts.clone(),
            _ => vec![],
        };
        let rest = ps.iter().position(|p| matches!(p.kind, PatternKind::Rest));
        let mut out = vec![];
        for (i, p) in ps.iter().enumerate() {
            if matches!(p.kind, PatternKind::Rest) {
                continue;
            }
            let idx = match rest {
                Some(r) if i > r => ts.len().saturating_sub(ps.len() - i),
                _ => i,
            };
            out.push((p, idx, ts.get(idx).cloned().unwrap_or(Ty::Unknown)));
        }
        out
    }

    /// The variant a path names, given the scrutinee type: `(enum, variant, index)`.
    fn variant_of(&self, res: &Res, ty: &Ty) -> Option<(DefId, DefId, usize)> {
        match res {
            Res::Def(d) => match self.r.def(*d).kind {
                DefKind::Variant { parent, index } => Some((parent, *d, index)),
                _ => None,
            },
            Res::Assoc { name, .. } => match ty {
                Ty::Adt(e, _) => {
                    let vs = self.r.variants(*e);
                    let i = vs.iter().position(|v| self.r.def(*v).name == *name)?;
                    Some((*e, vs[i], i))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Field-name prefix, field types, and variant for a constructor pattern path.
    fn ctor_fields(&mut self, path_span: Span, ty: &Ty) -> CtorFields {
        let v = self.variant_of(&self.r.path(path_span), ty);
        let prefix = v
            .map(|(_, v, _)| format!("{}.", self.r.def(v).name))
            .unwrap_or_default();
        (
            prefix,
            self.typed.adt_fields(ty, v.map(|x| x.1)),
            v.map(|(e, _, i)| (e, i)),
        )
    }

    /// Jumps to `fail` unless the variant of enum place `place` is `index`.
    fn test_variant(&mut self, place: &Place, index: usize, fail: BlockId, span: Span) {
        let d = self.temp_of(
            usize_ty(),
            Rvalue::Pure(Op::Discriminant, vec![Operand::Copy(place.clone())]),
            span,
        );
        let k = Operand::constant(ConstKind::Int(index as i128), usize_ty());
        let c = self.temp_of(
            bool_ty(),
            Rvalue::Pure(Op::Bin(BinOp::Eq), vec![d, k]),
            span,
        );
        let ok = self.new_block();
        self.switch(c, ok, fail, span);
        self.cur = ok;
    }

    /// Jumps to `fail` unless `cond` holds.
    fn test_cond(&mut self, op: BinOp, a: Operand, b: Operand, fail: BlockId, span: Span) {
        let c = self.temp_of(bool_ty(), Rvalue::Pure(Op::Bin(op), vec![a, b]), span);
        let ok = self.new_block();
        self.switch(c, ok, fail, span);
        self.cur = ok;
    }

    /// Emits tests that continue in the current block when `pat` matches the
    /// value at `place`, and jump to `fail` otherwise. Binds nothing.
    fn test_pattern(&mut self, pat: &Pattern, place: Place, ty: &Ty, fail: BlockId) {
        let span = pat.span;
        let is_binding = |p: &Pattern| match &p.kind {
            PatternKind::Ident { name, .. } => self.r.bindings.contains_key(&name.span),
            _ => false,
        };
        let refutable_inner = match &pat.kind {
            PatternKind::Wild
            | PatternKind::Rest
            | PatternKind::Ref { .. }
            | PatternKind::Or(_) => false,
            PatternKind::Ident { .. } => !is_binding(pat),
            // A string literal is itself a `&str`: compare against the reference.
            PatternKind::Lit(Lit::Str(..)) => {
                !matches!(ty, Ty::Ref(_, t) if matches!(**t, Ty::Prim(PrimTy::Str)))
            }
            _ => true,
        };
        if refutable_inner {
            if let Ty::Ref(m, inner) = ty {
                // Match ergonomics: look through references.
                let p = place.project(Proj::Deref(DerefKind::Ref(*m).into()));
                return self.test_pattern(pat, p, inner, fail);
            }
        }
        match &pat.kind {
            PatternKind::Wild | PatternKind::Rest => {}
            PatternKind::Ident { name, sub, .. } => {
                if self.r.bindings.contains_key(&name.span) {
                    if let Some(s) = sub {
                        self.test_pattern(s, place, ty, fail);
                    }
                } else if let Some((_, _, i)) = self.variant_of(&self.r.path(name.span), ty) {
                    self.test_variant(&place, i, fail, span);
                }
            }
            PatternKind::Lit(l) => {
                let k = self.lit(l, ty.clone());
                self.test_cond(BinOp::Eq, Operand::Copy(place), k, fail, span);
            }
            PatternKind::Range { lo, hi, inclusive } => {
                if let PatternKind::Lit(l) = &lo.kind {
                    let k = self.lit(l, ty.clone());
                    self.test_cond(BinOp::Ge, Operand::Copy(place.clone()), k, fail, span);
                }
                if let PatternKind::Lit(l) = &hi.kind {
                    let k = self.lit(l, ty.clone());
                    let op = if *inclusive { BinOp::Le } else { BinOp::Lt };
                    self.test_cond(op, Operand::Copy(place), k, fail, span);
                }
            }
            PatternKind::Tuple(ps) => {
                for (p, idx, et) in self.tuple_elems(ps, ty) {
                    self.test_pattern(p, place.field(idx.to_string()), &et, fail);
                }
            }
            PatternKind::TupleStruct(path, ps) => {
                let (prefix, fields, v) = self.ctor_fields(path.span, ty);
                if let Some((_, i)) = v {
                    self.test_variant(&place, i, fail, span);
                }
                for (i, p) in ps.iter().enumerate() {
                    let (n, ft) = fields
                        .get(i)
                        .cloned()
                        .unwrap_or((i.to_string(), Ty::Unknown));
                    self.test_pattern(p, place.field(format!("{prefix}{n}")), &ft, fail);
                }
            }
            PatternKind::Struct {
                path, fields: fps, ..
            } => {
                let (prefix, fields, v) = self.ctor_fields(path.span, ty);
                if let Some((_, i)) = v {
                    self.test_variant(&place, i, fail, span);
                }
                for fp in fps {
                    let ft = fields
                        .iter()
                        .find(|(n, _)| *n == fp.name.name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Ty::Unknown);
                    self.test_pattern(
                        &fp.pat,
                        place.field(format!("{prefix}{}", fp.name.name)),
                        &ft,
                        fail,
                    );
                }
            }
            PatternKind::Path(p) => {
                if let Some((_, _, i)) = self.variant_of(&self.r.path(p.span), ty) {
                    self.test_variant(&place, i, fail, span);
                }
            }
            PatternKind::Ref { mutable, inner } => {
                let it = match ty {
                    Ty::Ref(_, t) => (**t).clone(),
                    _ => Ty::Unknown,
                };
                self.test_pattern(
                    inner,
                    place.project(Proj::Deref(DerefKind::Ref(*mutable).into())),
                    &it,
                    fail,
                );
            }
            PatternKind::Slice(_) => self.unsupported("slice patterns", span),
            PatternKind::Or(alts) => {
                let ok = self.new_block();
                for alt in alts {
                    let next = self.new_block();
                    self.test_pattern(alt, place.clone(), ty, next);
                    self.goto(ok, span);
                    self.cur = next;
                }
                self.goto(fail, span);
                self.cur = ok;
            }
        }
    }

    fn lit(&self, l: &Lit, ty: Ty) -> Operand {
        let ty = match ty {
            Ty::Ref(_, t) if !matches!(*t, Ty::Prim(PrimTy::Str)) => *t,
            t => t,
        };
        let kind = match l {
            Lit::Int {
                value, negative, ..
            } => {
                if matches!(ty, Ty::Prim(PrimTy::F32 | PrimTy::F64)) {
                    let v = *value as f64;
                    ConstKind::Float(if *negative { -v } else { v })
                } else {
                    let v = *value as i128;
                    ConstKind::Int(if *negative { -v } else { v })
                }
            }
            Lit::Float { value, .. } => ConstKind::Float(*value),
            Lit::Str(s) => ConstKind::Str(s.clone()),
            Lit::Char(c) => ConstKind::Char(*c),
            Lit::Bool(b) => ConstKind::Bool(*b),
        };
        Operand::constant(kind, ty)
    }

    // ----- places ----------------------------------------------------------------------

    /// Adds deref projections for each reference/box layer of `ty`, returning the inner type.
    fn autoderef(&mut self, mut p: Place, mut ty: Ty, max: usize) -> (Place, Ty) {
        for _ in 0..max {
            match self.deref_once(p.clone(), &ty, Span::default()) {
                Some((np, nt)) => {
                    p = np;
                    ty = nt;
                }
                None => break,
            }
        }
        (p, ty)
    }

    /// Auto-dereferences `p` while `go_on` says the current type is not yet
    /// the one wanted (at most 8 steps).
    fn autoderef_until(
        &mut self,
        mut p: Place,
        mut ty: Ty,
        span: Span,
        done: impl Fn(&Self, &Ty) -> bool,
    ) -> (Place, Ty) {
        for _ in 0..8 {
            if done(self, &ty) {
                break;
            }
            match self.deref_once(p.clone(), &ty, span) {
                Some((np, nt)) => {
                    p = np;
                    ty = nt;
                }
                None => break,
            }
        }
        (p, ty)
    }

    /// One dereference step: through `&T`, `*T`, `Box<T>`, or a user `Deref`
    /// impl (which calls `deref`, or `deref_mut` when the place is written).
    fn deref_once(&mut self, p: Place, ty: &Ty, span: Span) -> Option<(Place, Ty)> {
        match ty.clone() {
            Ty::Ref(m, inner) => Some((p.project(Proj::Deref(DerefKind::Ref(m).into())), *inner)),
            Ty::Ptr(_, inner) => Some((p.project(Proj::Deref(DerefKind::Ptr.into())), *inner)),
            Ty::Adt(d, args) if self.lang(d, "Box") => Some((
                p.project(Proj::Deref(DerefKind::Box.into())),
                args[0].clone(),
            )),
            t @ Ty::Adt(..) => {
                let mutable = self.want_mut && self.typed.deref_target(&t, true).is_some();
                let (target, tf) = self.typed.deref_target(&t, mutable)?;
                let r = self.temp(Ty::Ref(mutable, Box::new(t.clone())), span);
                self.assign(Place::local(r), Rvalue::Ref { mutable, place: p }, span);
                let res = self.temp(Ty::Ref(mutable, Box::new(target.clone())), span);
                let mut subs = HashMap::new();
                subs.insert("Self".to_string(), t);
                subs.insert("T".to_string(), target.clone());
                let rv = Rvalue::Call {
                    callee: Callee::Fn(tf, sorted(&subs)),
                    args: vec![Operand::Move(Place::local(r))],
                    flows: vec![0],
                };
                self.assign(Place::local(res), rv, span);
                Some((
                    Place::local(res).project(Proj::Deref(DerefKind::Ref(mutable).into())),
                    target,
                ))
            }
            _ => None,
        }
    }

    /// Whether `t` has a named field `name` (no autoderef).
    fn has_field(&self, t: &Ty, name: &str) -> bool {
        match t {
            Ty::Adt(..) => self
                .typed
                .adt_fields(t, None)
                .iter()
                .any(|(n, _)| n == name),
            Ty::Tuple(_) => name.parse::<usize>().is_ok(),
            _ => false,
        }
    }

    /// `&base[start..end]` (or `&mut`): the intrinsic `slice_range(&base, start, end, flags)`
    /// where flags bit 0 = has end, bit 1 = inclusive. `base` is auto-dereferenced to
    /// the indexed value (`str`, `String`, `Vec<T>`, `[T]`, `[T; N]`).
    #[allow(clippy::too_many_arguments)]
    fn slice_range(
        &mut self,
        e: &Expr,
        base: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
        mutable: bool,
    ) -> Operand {
        let ty = self.ty(e);
        self.slice_range_typed(base, start, end, inclusive, mutable, ty, e.span)
    }

    /// [`Self::slice_range`] with the result type and span given.
    #[allow(clippy::too_many_arguments)]
    fn slice_range_typed(
        &mut self,
        base: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
        mutable: bool,
        ty: Ty,
        span: Span,
    ) -> Operand {
        let usize_ty = Ty::Prim(kiln_resolve::PrimTy::Usize);
        let s = match start {
            Some(x) => self.expr(x),
            None => Operand::constant(ConstKind::Int(0), usize_ty.clone()),
        };
        let en = match end {
            Some(x) => self.expr(x),
            None => Operand::constant(ConstKind::Int(0), usize_ty.clone()),
        };
        let flags = end.is_some() as i128 | ((inclusive as i128) << 1);
        let bt = self.ty(base);
        let bp = self.place_or_temp(base);
        // Strip references (but not String/Vec, which the intrinsic understands).
        let mut n = 0;
        let mut t = bt.clone();
        while let Ty::Ref(_, inner) = t {
            t = *inner;
            n += 1;
        }
        let (p, pt) = self.autoderef(bp, bt, n);
        let rt = Ty::Ref(mutable, Box::new(pt));
        let r = self.temp_of(rt, Rvalue::Ref { mutable, place: p }, span);
        let rv = Rvalue::Call {
            callee: Callee::Intrinsic("slice_range".into()),
            args: vec![r, s, en, Operand::constant(ConstKind::Int(flags), usize_ty)],
            flows: vec![0],
        };
        self.temp_of(ty, rv, span)
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
                let n = name.name.clone();
                let (p, _) = self.autoderef_until(bp, bt, e.span, |s, t| s.has_field(t, &n));
                Some(p.field(name.name.clone()))
            }
            ExprKind::TupleIndex(base, i) => {
                let bt = self.ty(base);
                let bp = self.place_or_temp(base);
                let n = i.to_string();
                let (p, _) = self.autoderef_until(bp, bt, e.span, |s, t| s.has_field(t, &n));
                Some(p.field(i.to_string()))
            }
            ExprKind::Index(base, idx) if matches!(idx.kind, ExprKind::Range { .. }) => {
                // `x[a..b]` as a place: the sub-slice behind a fresh reference.
                let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &idx.kind
                else {
                    unreachable!()
                };
                let m = self.want_mut;
                let elem = self.ty(e);
                let rt = Ty::Ref(m, Box::new(elem));
                let r = self.slice_range_typed(base, start, end, *inclusive, m, rt.clone(), e.span);
                let l = self.temp(rt, e.span);
                self.assign(Place::local(l), Rvalue::Use(r), e.span);
                Some(Place::local(l).project(Proj::Deref(DerefKind::Ref(m).into())))
            }
            ExprKind::Index(base, idx) => {
                let saved = std::mem::replace(&mut self.want_mut, false);
                let iv = self.expr(idx);
                self.want_mut = saved;
                let it = self.ty(idx);
                let i = self.temp(it, idx.span);
                self.assign(Place::local(i), Rvalue::Use(iv), idx.span);
                let bt = self.ty(base);
                let bp = self.place_or_temp(base);
                let (p, _) = self.autoderef_until(bp, bt, e.span, |s, t| match t {
                    Ty::Array(..) | Ty::Slice(_) => true,
                    Ty::Adt(d, _) => s.lang(*d, "Vec") || s.lang(*d, "String"),
                    _ => false,
                });
                Some(p.project(Proj::Index(i)))
            }
            ExprKind::Unary(ast::UnOp::Deref, x) => {
                let xt = self.ty(x);
                let xp = self.place_or_temp(x);
                let (p, _) = self.deref_once(xp.clone(), &xt, e.span).unwrap_or((xp, xt));
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
        self.assign(Place::local(t), rv, span);
        self.operand_of(Place::local(t), &ty)
    }

    /// Lowers `e` to an operand, applying any implicit coercion recorded by the type checker.
    fn expr(&mut self, e: &Expr) -> Operand {
        let op = self.expr_inner(e);
        if let Some(target) = self.res.coercions.get(&e.span).cloned() {
            // An interpolated `String` used as `&str`: keep it in a temporary
            // and borrow that.
            let src = self.ty(e);
            if matches!(&src, Ty::Adt(d, _) if self.lang(*d, "String"))
                && matches!(target, Ty::Ref(..))
            {
                let t = self.temp(src.clone(), e.span);
                self.assign(Place::local(t), Rvalue::Use(op), e.span);
                let r = self.temp_of(
                    Ty::Ref(false, Box::new(src)),
                    Rvalue::Ref {
                        mutable: false,
                        place: Place::local(t),
                    },
                    e.span,
                );
                return self.temp_of(target.clone(), Rvalue::Coerce(r, target), e.span);
            }
            if !matches!(&op, Operand::Const(c) if c.kind == ConstKind::Unit) {
                return self.temp_of(target.clone(), Rvalue::Coerce(op, target), e.span);
            }
        }
        op
    }

    fn expr_inner(&mut self, e: &Expr) -> Operand {
        let ty = self.ty(e);
        match &e.kind {
            ExprKind::Paren(x) => self.expr_inner(x),
            ExprKind::Path(p) => {
                if let Some(pl) = self.place(e) {
                    return self.operand_of(pl, &ty);
                }
                self.path_value(e, p, ty)
            }
            ExprKind::Field(..)
            | ExprKind::TupleIndex(..)
            | ExprKind::Index(..)
            | ExprKind::Unary(ast::UnOp::Deref, _) => match self.place(e) {
                Some(p) => self.operand_of(p, &ty),
                None => Operand::unit(),
            },
            ExprKind::Lit(l) => self.lit(l, ty),
            ExprKind::Error => Operand::unit(),
            ExprKind::Interp(parts) => {
                let mut segs = vec![];
                let mut ops = vec![];
                for part in parts {
                    match part {
                        ast::InterpPart::Text(t) => segs.push(InterpSeg::Text(t.clone())),
                        ast::InterpPart::Expr(x) => {
                            segs.push(InterpSeg::Arg(ops.len()));
                            ops.push(self.read(x));
                        }
                    }
                }
                self.temp_of(ty, Rvalue::Pure(Op::Interp(segs), ops), e.span)
            }
            ExprKind::Unary(op, x) => match op {
                ast::UnOp::Ref | ast::UnOp::RefMut => {
                    // `&x[a..b]`: a bounds-checked sub-slice.
                    if let ExprKind::Index(base, idx) = &x.kind {
                        if let ExprKind::Range {
                            start,
                            end,
                            inclusive,
                        } = &idx.kind
                        {
                            return self.slice_range(
                                e,
                                base,
                                start,
                                end,
                                *inclusive,
                                *op == ast::UnOp::RefMut,
                            );
                        }
                    }
                    let saved = std::mem::replace(&mut self.want_mut, *op == ast::UnOp::RefMut);
                    let p = self.place_or_temp(x);
                    self.want_mut = saved;
                    let rv = Rvalue::Ref {
                        mutable: *op == ast::UnOp::RefMut,
                        place: p,
                    };
                    self.temp_of(ty, rv, e.span)
                }
                ast::UnOp::Recv => {
                    let o = self.read(x);
                    let rv = Rvalue::Call {
                        callee: Callee::Intrinsic("chan_recv".into()),
                        args: vec![o],
                        flows: vec![],
                    };
                    self.temp_of(ty, rv, e.span)
                }
                ast::UnOp::Neg => {
                    // `-123` is a constant (so `-2147483648` is a valid `i32`).
                    if let ExprKind::Lit(Lit::Int { value, .. }) = &x.kind {
                        if !ty.is_float_prim() {
                            return Operand::constant(ConstKind::Int(-(*value as i128)), ty);
                        }
                    }
                    let o = self.expr(x);
                    self.temp_of(ty, Rvalue::Pure(Op::Neg, vec![o]), e.span)
                }
                _ => {
                    let o = self.expr(x);
                    self.temp_of(ty, Rvalue::Pure(Op::Not, vec![o]), e.span)
                }
            },
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), l, r) => {
                // Short-circuit: `a && b` is `if a { b } else { false }`.
                let t = self.temp(bool_ty(), e.span);
                let a = self.expr(l);
                let (rhs, short) = (self.new_block(), self.new_block());
                let join = self.new_block();
                if *op == BinOp::And {
                    self.switch(a, rhs, short, l.span);
                } else {
                    self.switch(a, short, rhs, l.span);
                }
                self.cur = short;
                let k = Operand::constant(ConstKind::Bool(*op == BinOp::Or), bool_ty());
                self.assign(Place::local(t), Rvalue::Use(k), e.span);
                self.goto(join, e.span);
                self.cur = rhs;
                let b = self.expr(r);
                self.assign(Place::local(t), Rvalue::Use(b), e.span);
                self.goto(join, e.span);
                self.cur = join;
                Operand::Copy(Place::local(t))
            }
            ExprKind::Binary(op, l, r) => {
                use ast::BinOp::*;
                let (a, b) = if matches!(op, Eq | Ne | Lt | Le | Gt | Ge) {
                    (self.read(l), self.read(r))
                } else {
                    (self.expr(l), self.expr(r))
                };
                self.temp_of(ty, Rvalue::Pure(Op::Bin(*op), vec![a, b]), e.span)
            }
            ExprKind::Cast(x, _) => {
                let o = self.expr(x);
                self.temp_of(ty.clone(), Rvalue::Pure(Op::Cast(ty), vec![o]), e.span)
            }
            ExprKind::Call(f, args) => self.call(e, f, args),
            ExprKind::MethodCall { receiver, args, .. } => {
                let (callee, args, flows) = self.method_operands(e, receiver, args);
                self.finish_call(e, callee, args, flows)
            }
            ExprKind::Try(x) => self.try_expr(e, x, ty),
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let ops = [start, end]
                    .into_iter()
                    .flatten()
                    .map(|x| self.expr(x))
                    .collect();
                let kind = AggKind::Range {
                    has_start: start.is_some(),
                    has_end: end.is_some(),
                    inclusive: *inclusive,
                };
                self.temp_of(ty, Rvalue::Aggregate(kind, ops), e.span)
            }
            ExprKind::Tuple(xs) if xs.is_empty() => Operand::unit(),
            ExprKind::Tuple(xs) => {
                let ops = xs.iter().map(|x| self.expr(x)).collect();
                self.temp_of(ty, Rvalue::Aggregate(AggKind::Tuple, ops), e.span)
            }
            ExprKind::Array(xs) => {
                let ops = xs.iter().map(|x| self.expr(x)).collect();
                self.temp_of(ty, Rvalue::Aggregate(AggKind::Array, ops), e.span)
            }
            ExprKind::ArrayRepeat(x, _) => {
                let n = match &ty {
                    Ty::Array(_, n) => *n,
                    _ => 0,
                };
                let a = self.expr(x);
                self.temp_of(
                    ty,
                    Rvalue::Aggregate(AggKind::ArrayRepeat(n), vec![a]),
                    e.span,
                )
            }
            ExprKind::StructLit { path, fields, base } => {
                let v = self.variant_of(&self.r.path(path.span), &ty);
                let mut ops = vec![];
                let mut names = vec![];
                for f in fields {
                    ops.push(self.expr(&f.value));
                    names.push(f.name.name.clone());
                }
                if let Some(b) = base {
                    // `S { a: x, ..base }`: copy/move `base`, then overwrite the listed fields.
                    let bo = self.expr(b);
                    let t = self.temp(ty.clone(), e.span);
                    self.assign(Place::local(t), Rvalue::Use(bo), e.span);
                    for (n, o) in names.into_iter().zip(ops) {
                        self.assign(Place::local(t).field(n), Rvalue::Use(o), e.span);
                    }
                    return self.operand_of(Place::local(t), &ty);
                }
                let kind = match (v, &ty) {
                    (Some((def, _, index)), _) => AggKind::Variant {
                        def,
                        index,
                        fields: names,
                    },
                    (None, Ty::Adt(d, _)) => AggKind::Struct(*d, names),
                    _ => AggKind::Tuple,
                };
                self.temp_of(ty, Rvalue::Aggregate(kind, ops), e.span)
            }
            ExprKind::Closure {
                is_move,
                params,
                body,
                ..
            } => {
                let ok = std::mem::take(&mut self.flow_ok);
                let runs_once = std::mem::take(&mut self.spawn_arg);
                let ret = match &ty {
                    Ty::Fn(_, r, _) => (**r).clone(),
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
                self.assign(
                    Place::local(t),
                    Rvalue::Aggregate(AggKind::Closure(e.span), ops),
                    e.span,
                );
                if consumes {
                    self.once.insert(t);
                }
                if let Some(why) = self.closure_restriction(&ty, consumes && !runs_once) {
                    self.local_fns.insert(t, why);
                }
                self.flow_ok = ok;
                self.operand_of(Place::local(t), &ty)
            }
            _ => {
                if matches!(ty, Ty::Unit | Ty::Never | Ty::Unknown) {
                    self.expr_into(e, None);
                    Operand::unit()
                } else {
                    let t = self.temp(ty.clone(), e.span);
                    self.expr_into(e, Some(Place::local(t)));
                    self.operand_of(Place::local(t), &ty)
                }
            }
        }
    }

    /// A path used as a value that is not a local: function items, unit variants,
    /// unit structs, constants.
    fn path_value(&mut self, e: &Expr, p: &ast::Path, ty: Ty) -> Operand {
        let res = self.r.path(p.span);
        if let Some(i) = self.res.callees.get(&e.span).copied() {
            let substs = self
                .res
                .callee_substs
                .get(&e.span)
                .map(sorted)
                .unwrap_or_default();
            return Operand::constant(ConstKind::Fn(i, substs), ty);
        }
        if let Some((def, _, index)) = self.variant_of(&res, &ty) {
            let kind = AggKind::Variant {
                def,
                index,
                fields: vec![],
            };
            return self.temp_of(ty, Rvalue::Aggregate(kind, vec![]), e.span);
        }
        if let Res::Def(d) = res {
            let def = self.r.def(d);
            match def.kind {
                DefKind::Struct => {
                    return self.temp_of(
                        ty,
                        Rvalue::Aggregate(AggKind::Struct(d, vec![]), vec![]),
                        e.span,
                    )
                }
                DefKind::Const => {
                    if let Some(ast::ItemKind::Const { value: Some(v), .. }) =
                        def.item.map(|i| &i.kind)
                    {
                        if let ExprKind::Lit(l) = &v.kind {
                            return self.lit(l, ty);
                        }
                    }
                    self.unsupported("constants with non-literal values", e.span);
                }
                DefKind::Static => self.unsupported("statics", e.span),
                _ => {}
            }
        }
        Operand::unit()
    }

    /// `x?`: on `Err(e)`/`None`, return `Err(From::from(e))`/`None`; otherwise unwrap.
    fn try_expr(&mut self, e: &Expr, x: &Expr, ty: Ty) -> Operand {
        let xt = self.ty(x);
        let inner = self.expr(x);
        let t = self.temp(xt.clone(), x.span);
        self.assign(Place::local(t), Rvalue::Use(inner), x.span);
        let Ty::Adt(d, args) = xt.clone() else {
            return Operand::unit();
        };
        let is_result = self.lang(d, "Result");
        let vs = self.r.variants(d);
        let (ok_name, err_index) = if is_result { ("Ok", 1) } else { ("Some", 1) };
        let ok = self.new_block();
        let err = self.new_block();
        let disc = self.temp_of(
            usize_ty(),
            Rvalue::Pure(Op::Discriminant, vec![Operand::Copy(Place::local(t))]),
            e.span,
        );
        let zero = Operand::constant(ConstKind::Int(0), usize_ty());
        let c = self.temp_of(
            bool_ty(),
            Rvalue::Pure(Op::Bin(BinOp::Eq), vec![disc, zero]),
            e.span,
        );
        self.switch(c, ok, err, e.span);
        self.cur = err;
        let ret_ty = self.body.locals[0].ty.clone();
        let (rd, rargs) = match &ret_ty {
            Ty::Adt(rd, ra) => (*rd, ra.clone()),
            _ => (d, args.clone()),
        };
        let rvs = self.r.variants(rd);
        if is_result {
            let e1 = args.get(1).cloned().unwrap_or(Ty::Unknown);
            let e2 = rargs.get(1).cloned().unwrap_or(Ty::Unknown);
            let mut payload = self.operand_of(Place::local(t).field("Err.0"), &e1);
            if e1 != e2 {
                if let Some(from) = self.r.prelude_type("From") {
                    if let Some(&f) = self.typed.trait_methods(from).first() {
                        let substs = vec![
                            ("Self".to_string(), e2.clone()),
                            ("T".to_string(), e1.clone()),
                        ];
                        payload = self.temp_of(
                            e2.clone(),
                            Rvalue::Call {
                                callee: Callee::Fn(f, substs),
                                args: vec![payload],
                                flows: vec![0],
                            },
                            e.span,
                        );
                    }
                }
            }
            let kind = AggKind::Variant {
                def: rd,
                index: err_index,
                fields: vec!["0".into()],
            };
            self.assign(
                Place::local(Local(0)),
                Rvalue::Aggregate(kind, vec![payload]),
                e.span,
            );
        } else {
            let kind = AggKind::Variant {
                def: rd,
                index: err_index,
                fields: vec![],
            };
            self.assign(
                Place::local(Local(0)),
                Rvalue::Aggregate(kind, vec![]),
                e.span,
            );
        }
        let _ = (vs, rvs);
        self.exit_scopes(1, e.span);
        self.goto(self.exit, e.span);
        self.cur = ok;
        self.operand_of(Place::local(t).field(format!("{ok_name}.0")), &ty)
    }

    /// A call argument. Passing a `&mut` place reborrows it (`&mut *r`) instead
    /// of moving the reference, so `r` stays usable after the call.
    fn arg(&mut self, a: &Expr) -> Operand {
        let ty = self.ty(a);
        if let Ty::Ref(true, _) = ty {
            if !self.res.coercions.contains_key(&a.span) {
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
        }
        self.expr(a)
    }

    fn print_kind(&self, f: &Expr) -> Option<Op> {
        let ExprKind::Path(p) = &f.kind else {
            return None;
        };
        let Res::Def(d) = self.r.path(p.span) else {
            return None;
        };
        if !self.r.is_core(d) {
            return None;
        }
        let def = self.r.def(d);
        match def.name.as_str() {
            "print" => Some(Op::Print {
                newline: false,
                stderr: false,
            }),
            "println" => Some(Op::Print {
                newline: true,
                stderr: false,
            }),
            "eprint" => Some(Op::Print {
                newline: false,
                stderr: true,
            }),
            "eprintln" => Some(Op::Print {
                newline: true,
                stderr: true,
            }),
            _ => None,
        }
    }

    /// `go f(args)` / `go [move] { ... }`: starts a task and returns its `Task<T>`.
    ///
    /// Both forms lower to a nested body (kind [`BodyKind::GoBlock`]) whose
    /// captures move into the task. For `go f(args)` the body is synthesized:
    /// the callee and arguments are evaluated now and captured, and the body
    /// makes the call.
    fn go(&mut self, e: &Expr, g: &GoTarget) -> Operand {
        let ty = self.ty(e);
        let ret = match &ty {
            Ty::Adt(_, a) if !a.is_empty() => a[0].clone(),
            _ => Ty::Unit,
        };
        let (kind, operands, captures) = match g {
            GoTarget::Call(call) => {
                let (callee, ops, flows) = match &call.kind {
                    ExprKind::Call(f, args) => self.call_operands(f, args),
                    ExprKind::MethodCall { receiver, args, .. } => {
                        self.method_operands(call, receiver, args)
                    }
                    _ => (Callee::Intrinsic("unknown".into()), vec![], vec![]),
                };
                let tys: Vec<Ty> = ops.iter().map(|o| self.operand_ty(o)).collect();
                let name = format!("{}::{{go}}", self.body.name);
                let res = self.res.clone();
                let mut inner = Lower::new(&mut *self.typed, res, name, BodyKind::GoBlock, e.span);
                inner.new_local(ret.clone(), None, e.span, LocalKind::Return, true, None);
                let ups: Vec<Local> = tys
                    .iter()
                    .map(|t| {
                        inner.new_local(t.clone(), None, call.span, LocalKind::Upvar, false, None)
                    })
                    .collect();
                let args = ups
                    .iter()
                    .zip(&tys)
                    .map(|(u, t)| inner.operand_of(Place::local(*u), t))
                    .collect();
                inner.assign(
                    Place::local(Local(0)),
                    Rvalue::Call {
                        callee,
                        args,
                        flows,
                    },
                    call.span,
                );
                let end = Span::new(e.span.end.saturating_sub(1), e.span.end);
                let (mut nb, _, extra) = inner.finish(end);
                nb.captures = ups.iter().map(|u| (*u, CaptureMode::Move)).collect();
                self.extra.push(nb);
                self.extra.extend(extra);
                (SpawnKind::GoCall, ops, vec![])
            }
            GoTarget::Block { is_move, body } => {
                let (ops, caps) = self.nested(
                    &[],
                    None,
                    NestedBody::Block(body),
                    *is_move,
                    BodyKind::GoBlock,
                    e.span,
                    ret,
                );
                (SpawnKind::GoBlock { is_move: *is_move }, ops, caps)
            }
        };
        self.push(
            StmtKind::Spawn {
                kind,
                operands,
                captures,
                body: e.span,
            },
            e.span,
        );
        let rv = Rvalue::Call {
            callee: Callee::Intrinsic("go_handle".into()),
            args: vec![],
            flows: vec![],
        };
        self.temp_of(ty, rv, e.span)
    }

    /// `select { ... }`: channel operands and sent values are evaluated first,
    /// in order; the runtime picks a ready case (or `_` when none is) and
    /// returns its index; the arms are then chosen by comparing the index.
    fn select(&mut self, arms: &[ast::SelectArm], span: Span) {
        let mut ops = vec![];
        let mut kinds = String::new();
        // Per arm: the case index and the receive buffer or sent value.
        let mut cases: Vec<(usize, Option<Local>)> = vec![];
        let mut default = None;
        for arm in arms {
            match &arm.op {
                SelectOp::Recv { chan, .. } => {
                    let mut ct = self.ty(chan);
                    while let Ty::Ref(_, x) = ct {
                        ct = *x;
                    }
                    let et = match ct {
                        Ty::ChanRecv(t) => *t,
                        _ => Ty::Unknown,
                    };
                    let c = self.read(chan);
                    // The runtime writes the received value straight into `buf`.
                    let buf = self.temp(et, chan.span);
                    cases.push((kinds.len(), Some(buf)));
                    kinds.push('r');
                    ops.push(c);
                    ops.push(Operand::Copy(Place::local(buf)));
                }
                SelectOp::Send { chan, value } => {
                    let c = self.read(chan);
                    let vt = self.ty(value);
                    let v = self.expr(value);
                    let sv = self.temp(vt, value.span);
                    self.assign(Place::local(sv), Rvalue::Use(v), value.span);
                    cases.push((kinds.len(), Some(sv)));
                    kinds.push('s');
                    ops.push(c);
                    ops.push(Operand::Copy(Place::local(sv)));
                }
                SelectOp::Default => {
                    default = Some(cases.len());
                    cases.push((usize::MAX, None));
                }
            }
        }
        let n = kinds.len();
        let name = format!("select:{kinds}:{}", default.is_some() as u8);
        let idx = self.temp_of(
            usize_ty(),
            Rvalue::Call {
                callee: Callee::Intrinsic(name),
                args: ops,
                flows: vec![],
            },
            span,
        );
        let join = self.new_block();
        for (arm, (k, local)) in arms.iter().zip(cases) {
            let k = if k == usize::MAX { n } else { k };
            let (armb, next) = (self.new_block(), self.new_block());
            let c = self.temp_of(
                bool_ty(),
                Rvalue::Pure(
                    Op::Bin(BinOp::Eq),
                    vec![
                        idx.clone(),
                        Operand::constant(ConstKind::Int(k as i128), usize_ty()),
                    ],
                ),
                arm.span,
            );
            self.switch(c, armb, next, arm.span);
            self.cur = armb;
            self.push_scope();
            match (&arm.op, local) {
                (SelectOp::Recv { bind, chan }, Some(buf)) => {
                    let id = bind
                        .as_ref()
                        .and_then(|b| b.name.as_ref())
                        .and_then(|n| self.r.bindings.get(&n.span));
                    let v = Operand::Move(Place::local(buf));
                    match id {
                        Some(&id) => {
                            let l = self.declare(id);
                            self.assign(Place::local(l), Rvalue::Use(v), chan.span);
                        }
                        // Not bound: the value is dropped.
                        None => self.push(StmtKind::Eval(Rvalue::Use(v)), chan.span),
                    }
                }
                (SelectOp::Send { .. }, Some(sv)) => {
                    // The runtime moved the value into the channel.
                    let rv = Rvalue::Call {
                        callee: Callee::Intrinsic("select_sent".into()),
                        args: vec![Operand::Move(Place::local(sv))],
                        flows: vec![],
                    };
                    self.push(StmtKind::Eval(rv), arm.span);
                }
                _ => {}
            }
            self.expr_into(&arm.body, None);
            self.pop_scope(arm.span);
            self.goto(join, arm.span);
            self.cur = next;
        }
        self.terminate(Terminator::Unreachable, span);
        self.cur = join;
    }

    /// For `defer f(args)` and `defer x.m(args)`: evaluates the callee and
    /// arguments now (Go semantics) into a tuple local that lives until
    /// function exit. Other deferred expressions (blocks) run at exit as written.
    fn defer_now(&mut self, e: &Expr) -> Option<(Local, DeferCall)> {
        let mut inner = e;
        while let ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let (ops, call) = match &inner.kind {
            ExprKind::Call(f, args) => {
                if let Some(op) = self.print_kind(f) {
                    // Printing borrows a named string instead of moving it.
                    let ops = args
                        .iter()
                        .map(|a| match self.place(a) {
                            Some(p) => {
                                let t = Ty::Ref(false, Box::new(self.ty(a)));
                                self.temp_of(
                                    t,
                                    Rvalue::Ref {
                                        mutable: false,
                                        place: p,
                                    },
                                    a.span,
                                )
                            }
                            None => self.expr(a),
                        })
                        .collect();
                    (ops, DeferCall::Print(op))
                } else {
                    let is_ctor = matches!(&f.kind, ExprKind::Path(_))
                        && !self.res.callees.contains_key(&f.span)
                        && !matches!(&f.kind, ExprKind::Path(p) if matches!(self.r.path(p.span), Res::Builtin(_)));
                    if is_ctor {
                        return None;
                    }
                    let (callee, ops, flows) = self.call_operands(f, args);
                    (ops, DeferCall::Call(callee, flows))
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                let (callee, ops, flows) = self.method_operands(inner, receiver, args);
                (ops, DeferCall::Call(callee, flows))
            }
            _ => return None,
        };
        let tys: Vec<Ty> = ops.iter().map(|o| self.operand_ty(o)).collect();
        let rec = self.new_local(Ty::Tuple(tys), None, e.span, LocalKind::Temp, true, None);
        // Lives until function exit (the outermost scope).
        self.scopes[0].push(rec);
        self.assign(
            Place::local(rec),
            Rvalue::Aggregate(AggKind::Tuple, ops),
            e.span,
        );
        Some((rec, call))
    }

    /// The body of a deferred call evaluated by [`Self::defer_now`].
    fn run_deferred(&mut self, rec: Local, call: &DeferCall, span: Span) {
        self.push(StmtKind::DeferLoad(rec), span);
        let Ty::Tuple(tys) = self.body.locals[rec.0 as usize].ty.clone() else {
            return;
        };
        match call {
            DeferCall::Print(op) => {
                let ops = (0..tys.len())
                    .map(|i| Operand::Copy(Place::local(rec).field(i.to_string())))
                    .collect();
                self.push(StmtKind::Eval(Rvalue::Pure(op.clone(), ops)), span);
            }
            DeferCall::Call(callee, flows) => {
                let ops = tys
                    .iter()
                    .enumerate()
                    .map(|(i, t)| self.operand_of(Place::local(rec).field(i.to_string()), t))
                    .collect();
                let rv = Rvalue::Call {
                    callee: callee.clone(),
                    args: ops,
                    flows: flows.clone(),
                };
                self.push(StmtKind::Eval(rv), span);
            }
        }
    }

    /// The type of an operand.
    fn operand_ty(&self, o: &Operand) -> Ty {
        match o {
            Operand::Const(c) => c.ty.clone(),
            Operand::Copy(p) | Operand::Move(p) => self.place_ty(p),
        }
    }

    /// The type of a place: its local's type through each projection.
    fn place_ty(&self, p: &Place) -> Ty {
        let mut t = self.body.locals[p.local.0 as usize].ty.clone();
        for x in &p.proj {
            t = match (x, t) {
                (Proj::Deref(_), Ty::Ref(_, inner) | Ty::Ptr(_, inner)) => *inner,
                (Proj::Deref(_), Ty::Adt(d, a)) if self.lang(d, "Box") && !a.is_empty() => {
                    a[0].clone()
                }
                (Proj::Index(_), t) => self.elem_ty(&t),
                (Proj::Field(f), Ty::Tuple(ts)) => f
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| ts.get(i).cloned())
                    .unwrap_or(Ty::Unknown),
                (Proj::Field(f), t @ Ty::Adt(..)) => {
                    let (variant, name) = match f.split_once('.') {
                        Some((v, n)) => (Some(v), n),
                        None => (None, f.as_str()),
                    };
                    let vdef = match (&t, variant) {
                        (Ty::Adt(d, _), Some(v)) => self
                            .r
                            .variants(*d)
                            .into_iter()
                            .find(|x| self.r.def(*x).name == v),
                        _ => None,
                    };
                    self.typed
                        .adt_fields(&t, vdef)
                        .into_iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, t)| t)
                        .unwrap_or(Ty::Unknown)
                }
                _ => Ty::Unknown,
            };
        }
        t
    }

    /// Lowers callee and arguments; returns the callee, operands, and borrow flows.
    fn call_operands(&mut self, f: &Expr, args: &[Expr]) -> (Callee, Vec<Operand>, Vec<usize>) {
        if let ExprKind::Path(p) = &f.kind {
            match self.r.path(p.span) {
                Res::Builtin(Builtin::Chan) => {
                    return (
                        Callee::Intrinsic("chan".into()),
                        args.iter().map(|a| self.expr(a)).collect(),
                        vec![],
                    );
                }
                Res::Builtin(Builtin::Drop) => {
                    return (
                        Callee::Intrinsic("drop".into()),
                        args.iter().map(|a| self.expr(a)).collect(),
                        vec![],
                    );
                }
                _ => {}
            }
        }
        let callee_idx = self.res.callees.get(&f.span).copied();
        let is_item = matches!(&f.kind, ExprKind::Path(p) if matches!(self.r.path(p.span), Res::Def(_) | Res::Assoc { .. }));
        let mut ops = Vec::new();
        let callee = match callee_idx {
            Some(i) if is_item => Callee::Fn(
                i,
                self.res
                    .callee_substs
                    .get(&f.span)
                    .map(sorted)
                    .unwrap_or_default(),
            ),
            _ => {
                let c = match self.place(f) {
                    Some(p) if p.proj.is_empty() && self.once.contains(&p.local) => {
                        Operand::Move(p)
                    }
                    Some(p) => Operand::Copy(p),
                    None => {
                        self.flow_ok = Self::direct(f);
                        self.expr(f)
                    }
                };
                ops.push(c);
                Callee::Value
            }
        };
        let offset = ops.len();
        // `scope`'s body runs exactly once, so it may move captures out.
        let once_arg = callee_idx
            .is_some_and(|i| self.r.fn_is_core(i) && self.r.fns[i].decl.name.name == "scope");
        for a in args {
            self.spawn_arg = once_arg;
            let o = self.arg(a);
            ops.push(o);
        }
        self.spawn_arg = false;
        let flows = match callee_idx {
            Some(i) => self.fn_flows(i).into_iter().map(|x| x + offset).collect(),
            None => (0..ops.len()).collect(),
        };
        (callee, ops, flows)
    }

    fn fn_flows(&self, i: usize) -> Vec<usize> {
        let f = &self.r.fns[i];
        let og = f.impl_idx.map(|idx| &self.r.impls[idx].decl.generics);
        let outer = og.map(sig::type_param_names).unwrap_or_default();
        sig::decl_flows(f.decl, &outer, og, self.r)
    }

    fn call(&mut self, e: &Expr, f: &Expr, args: &[Expr]) -> Operand {
        let ty = self.ty(e);
        if let Some(op) = self.print_kind(f) {
            let ops = args.iter().map(|a| self.read(a)).collect();
            self.push(StmtKind::Eval(Rvalue::Pure(op, ops)), e.span);
            return Operand::unit();
        }
        // Tuple-struct and tuple-variant constructors.
        if let ExprKind::Path(p) = &f.kind {
            if !self.res.callees.contains_key(&f.span) {
                let res = self.r.path(p.span);
                let ctor = match (self.variant_of(&res, &ty), &res) {
                    (Some((def, _, index)), _) => Some(AggKind::Variant {
                        def,
                        index,
                        fields: (0..args.len()).map(|i| i.to_string()).collect(),
                    }),
                    (None, Res::Def(d)) if self.r.def(*d).kind == DefKind::Struct => Some(
                        AggKind::Struct(*d, (0..args.len()).map(|i| i.to_string()).collect()),
                    ),
                    _ => None,
                };
                if let Some(kind) = ctor {
                    let ops = args.iter().map(|a| self.expr(a)).collect();
                    return self.temp_of(ty, Rvalue::Aggregate(kind, ops), e.span);
                }
            }
        }
        let (callee, ops, flows) = self.call_operands(f, args);
        self.finish_call(e, callee, ops, flows)
    }

    fn finish_call(
        &mut self,
        e: &Expr,
        callee: Callee,
        args: Vec<Operand>,
        flows: Vec<usize>,
    ) -> Operand {
        let ty = self.ty(e);
        let rv = Rvalue::Call {
            callee,
            args,
            flows,
        };
        if matches!(ty, Ty::Unit | Ty::Never) {
            self.push(StmtKind::Eval(rv), e.span);
            if ty == Ty::Never {
                self.terminate(Terminator::Unreachable, e.span);
                self.cur = self.new_block();
            }
            return Operand::unit();
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
    ) -> (Callee, Vec<Operand>, Vec<usize>) {
        let info = self.res.methods.get(&e.span).cloned();
        let base = match self.place(recv) {
            Some(p) => p,
            None => {
                let t = self.temp(self.ty(recv), recv.span);
                self.expr_into(recv, Some(Place::local(t)));
                Place::local(t)
            }
        };
        // A scoped task's closure runs exactly once, so it may move captures out.
        self.spawn_arg = info.as_ref().is_some_and(|info| {
            info.fn_idx
                .is_some_and(|i| self.r.fn_is_core(i) && self.r.fns[i].decl.name.name == "spawn")
        });
        let arg_ops: Vec<Operand> = args.iter().map(|a| self.arg(a)).collect();
        self.spawn_arg = false;
        let Some(info) = info else {
            let mut ops = vec![Operand::Copy(base)];
            ops.extend(arg_ops);
            let n = ops.len();
            return (Callee::Intrinsic("unknown".into()), ops, (0..n).collect());
        };
        let saved = std::mem::replace(
            &mut self.want_mut,
            matches!(
                info.self_param,
                Some(ast::SelfParam::Ref { mutable: true, .. })
            ),
        );
        let rt0 = self.ty(recv);
        let (p, pt) = self.autoderef(base, rt0, info.steps);
        self.want_mut = saved;
        // A `str` method on a `String` (or a slice method on a `Vec`): borrow
        // the container, then coerce the reference.
        let unsized_target = matches!(
            info.recv_ty,
            Ty::Prim(kiln_resolve::PrimTy::Str) | Ty::Slice(_)
        );
        let container =
            matches!(&pt, Ty::Adt(d, _) if self.lang(*d, "String") || self.lang(*d, "Vec"));
        let self_op = match &info.self_param {
            Some(ast::SelfParam::Ref { mutable, .. }) if unsized_target && container => {
                let r1 = self.temp_of(
                    Ty::Ref(*mutable, Box::new(pt.clone())),
                    Rvalue::Ref {
                        mutable: *mutable,
                        place: p,
                    },
                    recv.span,
                );
                let rt = Ty::Ref(*mutable, Box::new(info.recv_ty.clone()));
                self.temp_of(rt.clone(), Rvalue::Coerce(r1, rt), recv.span)
            }
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
        let name = match &e.kind {
            ExprKind::MethodCall { method, .. } => method.name.clone(),
            _ => String::new(),
        };
        let scoped = info.fn_idx.is_some_and(|i| {
            let f = &self.r.fns[i];
            self.r.fn_is_core(i) && f.decl.name.name == "spawn"
        });
        let mut ops = vec![self_op];
        ops.extend(arg_ops);
        let flows = match info.fn_idx {
            Some(i) => self.fn_flows(i),
            None => (0..ops.len()).collect(),
        };
        if scoped {
            // Checked by the borrow checker (captures must be `Send`/`Sync`);
            // the task itself is started by the `Scope::spawn` call.
            let captures = self.last_captures.take().unwrap_or_default();
            // The closure (and the loans it holds) must stay alive until the
            // scope's body returns, since the task may run until then.
            let operands = ops
                .get(1)
                .and_then(|o| o.place().cloned())
                .map(Operand::Copy)
                .into_iter()
                .collect();
            self.push(
                StmtKind::Spawn {
                    kind: SpawnKind::Scoped,
                    operands,
                    captures,
                    body: e.span,
                },
                e.span,
            );
        }
        let callee = match info.fn_idx {
            Some(i) => Callee::Fn(i, sorted(&info.substs)),
            None => Callee::Intrinsic(name),
        };
        (callee, ops, flows)
    }

    /// Lowers `e`, storing its value in `dest` (or discarding it).
    fn expr_into(&mut self, e: &Expr, dest: Option<Place>) {
        match &e.kind {
            ExprKind::Paren(x) => self.expr_into(x, dest),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.block_into(b, dest),
            ExprKind::If { cond, then, els } => {
                let c = self.expr(cond);
                let (tb, eb, join) = (self.new_block(), self.new_block(), self.new_block());
                self.switch(c, tb, eb, cond.span);
                self.cur = tb;
                self.block_into(then, dest.clone());
                self.goto(join, e.span);
                self.cur = eb;
                if let Some(x) = els {
                    self.expr_into(x, dest);
                }
                self.goto(join, e.span);
                self.cur = join;
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
                let (eb, join) = (self.new_block(), self.new_block());
                self.test_pattern(pat, p.clone(), &ty, eb);
                self.push_scope();
                self.bind_pattern(pat, p, &ty, None);
                self.block_into(then, dest.clone());
                self.pop_scope(Self::end_span(then));
                self.goto(join, e.span);
                self.cur = eb;
                if let Some(x) = els {
                    self.expr_into(x, dest);
                }
                self.goto(join, e.span);
                self.cur = join;
            }
            ExprKind::Match(scrut, arms) => {
                let ty = self.ty(scrut);
                let sp = self.place_or_temp(scrut);
                self.push(StmtKind::Read(sp.clone()), scrut.span);
                let join = self.new_block();
                let mut next = self.new_block();
                self.goto(next, e.span);
                for arm in arms {
                    self.cur = next;
                    next = self.new_block();
                    self.test_pattern(&arm.pat, sp.clone(), &ty, next);
                    self.push_scope();
                    self.bind_pattern(&arm.pat, sp.clone(), &ty, None);
                    if let Some(g) = &arm.guard {
                        let o = self.expr(g);
                        let body = self.new_block();
                        self.switch(o, body, next, g.span);
                        self.cur = body;
                    }
                    self.expr_into(&arm.body, dest.clone());
                    self.pop_scope(arm.span);
                    self.goto(join, arm.span);
                }
                self.cur = next;
                self.terminate(Terminator::Unreachable, e.span);
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
                    depth: self.scopes.len(),
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
                self.switch(c, bodyb, after, cond.span);
                self.cur = bodyb;
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: None,
                    depth: self.scopes.len(),
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
                let after = self.new_block();
                self.goto(head, e.span);
                self.cur = head;
                self.push_scope();
                let ty = self.ty(expr);
                let p = self.place_or_temp(expr);
                self.push(StmtKind::Read(p.clone()), expr.span);
                let fail = self.new_block();
                self.test_pattern(pat, p.clone(), &ty, fail);
                self.bind_pattern(pat, p, &ty, None);
                self.loops.push(LoopCx {
                    label: label.as_ref().map(|l| l.name.clone()),
                    brk: after,
                    cont: head,
                    result: None,
                    depth: self.scopes.len() - 1,
                });
                self.block_into(body, None);
                self.loops.pop();
                let end = Self::end_span(body);
                let scope = self.scopes.last().unwrap().clone();
                self.pop_scope(end);
                self.goto(head, e.span);
                // The scrutinee temporaries also die when the loop ends.
                self.cur = fail;
                for l in scope.into_iter().rev() {
                    self.push(StmtKind::StorageDead(l), end);
                }
                self.goto(after, e.span);
                self.cur = after;
            }
            ExprKind::For {
                label,
                pat,
                iter,
                body,
            } => self.for_loop(e, label, pat, iter, body),
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
                self.exit_scopes(self.loops[i].depth, e.span);
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
                    self.exit_scopes(self.loops[i].depth, e.span);
                    self.goto(c, e.span);
                    self.cur = self.new_block();
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr_into(v, Some(Place::local(Local(0))));
                }
                self.exit_scopes(1, e.span);
                self.goto(self.exit, e.span);
                self.cur = self.new_block();
            }
            ExprKind::Select(arms) => self.select(arms, e.span),
            ExprKind::Go(g) => {
                let op = self.go(e, g);
                match dest {
                    Some(d) => self.assign(d, Rvalue::Use(op), e.span),
                    // A `go` statement detaches the task.
                    None => self.push(StmtKind::Eval(Rvalue::Use(op)), e.span),
                }
            }
            _ => {
                self.flow_ok = dest.as_ref().is_some_and(|d| d.proj.is_empty()) && Self::direct(e);
                let op = self.expr(e);
                self.flow_ok = false;
                match dest {
                    Some(d) => {
                        if let Some(p) = op.place() {
                            if d.proj.is_empty() && p.proj.is_empty() {
                                if self.once.contains(&p.local) {
                                    self.once.insert(d.local);
                                }
                                if let Some(w) = self.local_fns.get(&p.local).cloned() {
                                    self.local_fns.insert(d.local, w);
                                }
                            }
                        }
                        self.assign(d, Rvalue::Use(op), e.span)
                    }
                    None => {
                        if !matches!(op, Operand::Const(_)) {
                            self.push(StmtKind::Eval(Rvalue::Use(op)), e.span)
                        }
                    }
                }
            }
        }
    }

    /// `for pat in iter { body }` over ranges, `&Vec<T>`/`&[T]` (by reference), and `Vec<T>` (by value).
    fn for_loop(
        &mut self,
        e: &Expr,
        label: &Option<ast::Ident>,
        pat: &Pattern,
        iter: &Expr,
        body: &Block,
    ) {
        let it_ty = self.ty(iter);
        let it_op = self.expr(iter);
        let it = self.temp(it_ty.clone(), iter.span);
        self.assign(Place::local(it), Rvalue::Use(it_op), iter.span);
        // By-value `Vec` iteration: the elements move into a non-owning view
        // (`cap` = usize::MAX) that advances as they are taken, and the vector
        // keeps only its buffer. Leaving the loop early drops the view, which
        // drops exactly the elements not taken yet.
        let by_value_vec = matches!(&it_ty, Ty::Adt(d, _) if self.lang(*d, "Vec"));
        let view = if by_value_vec {
            let v = self.temp(it_ty.clone(), iter.span);
            let rv = Rvalue::Call {
                callee: Callee::Intrinsic("vec_iter_view".into()),
                args: vec![Operand::Copy(Place::local(it))],
                flows: vec![],
            };
            self.assign(Place::local(v), rv, iter.span);
            let forget = Rvalue::Call {
                callee: Callee::Intrinsic("vec_forget_elems".into()),
                args: vec![Operand::Copy(Place::local(it))],
                flows: vec![],
            };
            self.push(StmtKind::Eval(forget), iter.span);
            Some(v)
        } else if let (Ty::Array(et, _), Some(vec)) = (&it_ty, self.r.prelude_type("Vec")) {
            // By-value arrays: the elements move into a view of the array's storage.
            let v = self.temp(Ty::Adt(vec, vec![(**et).clone()]), iter.span);
            let rv = Rvalue::Call {
                callee: Callee::Intrinsic("array_iter_view".into()),
                args: vec![Operand::Move(Place::local(it))],
                flows: vec![],
            };
            self.assign(Place::local(v), rv, iter.span);
            Some(v)
        } else {
            None
        };
        let head = self.new_block();
        let bodyb = self.new_block();
        let after = self.new_block();
        let exit_loop = self.new_block();
        self.goto(head, e.span);
        self.cur = head;
        let sp = iter.span;
        let one = |t: Ty| Operand::constant(ConstKind::Int(1), t);
        let cleanup: Option<Rvalue> = None;
        // Emits the condition and element; returns the element local and type.
        let (el, elem_ty) = match &it_ty {
            Ty::Adt(d, a) if self.lang(*d, "Range") => {
                let t = a[0].clone();
                let inclusive = matches!(
                    &iter.kind,
                    ExprKind::Range {
                        inclusive: true,
                        ..
                    }
                );
                let start = Place::local(it).field("start");
                let end = Place::local(it).field("end");
                let op = if inclusive { BinOp::Le } else { BinOp::Lt };
                let c = self.temp_of(
                    bool_ty(),
                    Rvalue::Pure(
                        Op::Bin(op),
                        vec![Operand::Copy(start.clone()), Operand::Copy(end)],
                    ),
                    sp,
                );
                self.switch(c, bodyb, exit_loop, sp);
                self.cur = bodyb;
                self.push_scope();
                let el = self.temp(t.clone(), pat.span);
                self.assign(
                    Place::local(el),
                    Rvalue::Use(Operand::Copy(start.clone())),
                    sp,
                );
                self.assign(
                    start.clone(),
                    Rvalue::Pure(
                        Op::Bin(BinOp::Add),
                        vec![Operand::Copy(start), one(t.clone())],
                    ),
                    sp,
                );
                (el, t)
            }
            Ty::ChanRecv(t) => {
                // `for v in rx`: receive until the channel is closed.
                let t = (**t).clone();
                let opt_ty = match self.r.prelude_type("Option") {
                    Some(o) => Ty::Adt(o, vec![t.clone()]),
                    None => Ty::Unknown,
                };
                let opt = self.temp(opt_ty, sp);
                self.assign(
                    Place::local(opt),
                    Rvalue::Call {
                        callee: Callee::Intrinsic("recv".into()),
                        args: vec![Operand::Copy(Place::local(it))],
                        flows: vec![],
                    },
                    sp,
                );
                // `Some` is variant 0.
                self.test_variant(&Place::local(opt), 0, exit_loop, sp);
                self.goto(bodyb, sp);
                self.cur = bodyb;
                self.push_scope();
                let el = self.temp(t.clone(), pat.span);
                self.assign(
                    Place::local(el),
                    Rvalue::Use(Operand::Move(Place::local(opt).field("Some.0"))),
                    sp,
                );
                (el, t)
            }
            _ => {
                // Index-based iteration.
                let (by_ref, inner) = match &it_ty {
                    Ty::Ref(m, inner) => (Some(*m), (**inner).clone()),
                    t => (None, t.clone()),
                };
                let et = match &inner {
                    Ty::Adt(d, a) if self.lang(*d, "Vec") => Some(a[0].clone()),
                    Ty::Slice(t) if by_ref.is_some() => Some((**t).clone()),
                    Ty::Array(t, _) => Some((**t).clone()),
                    _ => None,
                };
                let Some(et) = et else {
                    self.unsupported("this kind of `for` loop", iter.span);
                    self.terminate(Terminator::Branch(vec![bodyb, exit_loop]), sp);
                    self.cur = bodyb;
                    self.push_scope();
                    let et = self.elem_ty(&it_ty);
                    let el = self.temp(et.clone(), pat.span);
                    let rv = Rvalue::Call {
                        callee: Callee::Intrinsic("iter_next".into()),
                        args: vec![Operand::Copy(Place::local(it))],
                        flows: vec![0],
                    };
                    self.assign(Place::local(el), rv, pat.span);
                    return self
                        .finish_for(label, pat, body, e, el, et, head, after, exit_loop, None);
                };
                let idx = self.temp(usize_ty(), sp);
                self.assign_before_head(
                    idx,
                    Operand::constant(ConstKind::Int(0), usize_ty()),
                    sp,
                    head,
                );
                let len = self.temp_of(
                    usize_ty(),
                    Rvalue::Call {
                        callee: Callee::Intrinsic("len".into()),
                        args: vec![Operand::Copy(Place::local(view.unwrap_or(it)))],
                        flows: vec![],
                    },
                    sp,
                );
                let c = self.temp_of(
                    bool_ty(),
                    Rvalue::Pure(
                        Op::Bin(BinOp::Lt),
                        vec![Operand::Copy(Place::local(idx)), len],
                    ),
                    sp,
                );
                self.switch(c, bodyb, exit_loop, sp);
                self.cur = bodyb;
                self.push_scope();
                let (el, ety) = match by_ref {
                    Some(m) => {
                        let ety = Ty::Ref(m, Box::new(et.clone()));
                        let place = Place::local(it)
                            .project(Proj::Deref(DerefKind::Ref(m).into()))
                            .project(Proj::Index(idx));
                        let el = self.temp(ety.clone(), pat.span);
                        self.assign(Place::local(el), Rvalue::Ref { mutable: m, place }, sp);
                        (el, ety)
                    }
                    None => {
                        let el = self.temp(et.clone(), pat.span);
                        let rv = Rvalue::Call {
                            callee: Callee::Intrinsic("vec_take_front".into()),
                            args: vec![Operand::Copy(Place::local(view.unwrap_or(it)))],
                            flows: vec![0],
                        };
                        self.assign(Place::local(el), rv, sp);
                        // The view shrinks instead; the index stays 0.
                        return self.finish_for(
                            label, pat, body, e, el, et, head, after, exit_loop, cleanup,
                        );
                    }
                };
                self.assign(
                    Place::local(idx),
                    Rvalue::Pure(
                        Op::Bin(BinOp::Add),
                        vec![Operand::Copy(Place::local(idx)), one(usize_ty())],
                    ),
                    sp,
                );
                (el, ety)
            }
        };
        self.finish_for(
            label, pat, body, e, el, elem_ty, head, after, exit_loop, cleanup,
        )
    }

    /// Inserts `l = v` at the end of the block that jumps to `head` (the loop preheader).
    fn assign_before_head(&mut self, l: Local, v: Operand, span: Span, head: BlockId) {
        // The current block is `head`; the preheader is the block whose terminator is `goto head`
        // and that was current just before. Find it (the most recent such block).
        let pre = (0..self.body.blocks.len())
            .rev()
            .find(|&b| b as u32 != head.0 && self.body.blocks[b].term == Terminator::Goto(head));
        if let Some(b) = pre {
            self.body.blocks[b].stmts.push(Statement {
                kind: StmtKind::Assign(Place::local(l), Rvalue::Use(v)),
                span,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_for(
        &mut self,
        label: &Option<ast::Ident>,
        pat: &Pattern,
        body: &Block,
        e: &Expr,
        el: Local,
        elem_ty: Ty,
        head: BlockId,
        after: BlockId,
        exit_loop: BlockId,
        cleanup: Option<Rvalue>,
    ) {
        self.bind_pattern(pat, Place::local(el), &elem_ty, None);
        let depth = self.scopes.len() - 1;
        self.loops.push(LoopCx {
            depth,
            label: label.as_ref().map(|l| l.name.clone()),
            brk: after,
            cont: head,
            result: None,
        });
        self.block_into(body, None);
        self.loops.pop();
        self.pop_scope(Self::end_span(body));
        self.goto(head, e.span);
        self.cur = exit_loop;
        self.goto(after, e.span);
        self.cur = after;
        if let Some(rv) = cleanup {
            self.push(StmtKind::Eval(rv), e.span);
        }
    }

    fn elem_ty(&self, t: &Ty) -> Ty {
        match t {
            Ty::Adt(d, a) if (self.lang(*d, "Range") || self.lang(*d, "Vec")) && !a.is_empty() => {
                a[0].clone()
            }
            Ty::Array(x, _) | Ty::Slice(x) | Ty::ChanRecv(x) => (**x).clone(),
            Ty::Ref(m, inner) => match &**inner {
                Ty::Adt(d, a) if self.lang(*d, "Vec") => Ty::Ref(*m, Box::new(a[0].clone())),
                Ty::Array(x, _) | Ty::Slice(x) => Ty::Ref(*m, x.clone()),
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
            Some(Ty::Fn(ps, ..)) => ps.clone(),
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
        let (mut nb, upvars, extra) = inner.finish(end);
        let mut ops = vec![];
        let mut caps = vec![];
        self.last_consumes = false;
        for (id, ul) in &upvars {
            let inferred = capture_mode(&nb, *ul);
            if inferred == CaptureMode::Move {
                self.last_consumes = true;
            }
            let mode = if is_move { CaptureMode::Move } else { inferred };
            nb.captures.push((*ul, mode));
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
        if let Terminator::SwitchBool {
            cond: Operand::Move(p),
            ..
        } = &bb.term
        {
            if roots(p) {
                return CaptureMode::Move;
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
