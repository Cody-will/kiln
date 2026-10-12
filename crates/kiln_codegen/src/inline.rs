//! MIR inlining of small functions (release builds).
//!
//! Inlining happens per monomorphized instance, just before translation,
//! so the callee is known exactly: generic arguments are concrete and trait
//! methods are resolved to their implementation. A call
//! `dest = f(a, b)` whose callee is small and simple becomes:
//!
//! ```text
//!     p1' = a; p2' = b; goto entry'     (the callee's arguments)
//! entry': ...callee blocks...           (locals renumbered, types made concrete;
//!         return => goto cont)            `_0` becomes the fresh local ret')
//! cont:   dest = move ret'; ...rest of the original block...
//! ```
//!
//! Each piece keeps its meaning under the code generator's rules:
//! parameters and the return value become ordinary locals with drop flags,
//! which assignments set and moves clear. The callee's own `StorageDead`s
//! still drop its parameters, and `Eval` of a call drops `ret'` instead.
//!
//! Inlined callees have no loops, `defer`, closures, tasks, or `select`, and
//! at most [`MAX_STMTS`] statements; a body inlines at most [`MAX_INLINES`]
//! calls (which bounds recursion through inlined bodies too).

use crate::Codegen;
use kiln_mir::{
    AggKind, BlockData, BlockId, Body, BodyKind, Callee, ConstKind, Local, LocalKind, Op, Operand, Place, Proj,
    Rvalue, Statement, StmtKind, Terminator,
};
use kiln_typeck::ty::Ty;
use std::collections::HashMap;

/// Largest callee (in MIR statements) that is inlined.
pub(crate) const MAX_STMTS: usize = 40;
/// Most calls inlined into one body.
pub(crate) const MAX_INLINES: usize = 48;

type Substs = HashMap<String, Ty>;

impl Codegen<'_, '_> {
    /// The concrete function and substitution a call reaches, when it is a
    /// direct call of a function with a MIR body (not virtual, intrinsic, or
    /// extern). Unlike `FnCx::resolve`, declares nothing.
    fn static_target(&mut self, idx: usize, substs: Substs) -> Option<(usize, Substs)> {
        let r = self.r;
        let f = &r.fns[idx];
        if let (Some(_), None) = (f.trait_def, f.impl_idx) {
            if matches!(substs.get("Self"), Some(Ty::Dyn(_)) | None) {
                return None;
            }
            let (i2, s2) = self.typed.resolve_trait_method(idx, &substs)?;
            if i2 == idx {
                return r.fns[i2].decl.body.is_some().then_some((i2, s2));
            }
            let s2 = s2.into_iter().map(|(k, v)| (k, v.subst(&substs))).collect();
            return self.static_target(i2, s2);
        }
        f.decl.body.as_ref()?;
        Some((idx, substs))
    }

    /// Inlines small direct calls into `body` (an instance with `substs`).
    pub(crate) fn inline_calls(&mut self, body: &mut Body, substs: &Substs, self_idx: usize) {
        let mut count = 0;
        let mut bi = 0;
        while bi < body.blocks.len() && count < MAX_INLINES {
            let mut si = 0;
            let mut done = true;
            while si < body.blocks[bi].stmts.len() {
                let Some((callee, args)) = call_of(&body.blocks[bi].stmts[si].kind) else {
                    si += 1;
                    continue;
                };
                let Callee::Fn(i, cs) = callee else {
                    si += 1;
                    continue;
                };
                // Substitutions in this body may mention its own parameters
                // (inlined parts are already concrete; subst is a no-op there).
                let cs: Substs = cs.iter().map(|(k, v)| (k.clone(), v.subst(substs))).collect();
                let target = match self.static_target(*i, cs) {
                    Some((t, s)) if t != self_idx => (t, s),
                    _ => {
                        si += 1;
                        continue;
                    }
                };
                let callee_body = match self.bodies(target.0).first() {
                    Some(b) if inlinable(b) => b.clone(),
                    _ => {
                        si += 1;
                        continue;
                    }
                };
                let args = args.clone();
                splice(body, bi, si, callee_body, &target.1, args);
                count += 1;
                // Keep scanning the head of this block (now ending in a
                // jump); the callee's blocks and the continuation follow.
                done = false;
                break;
            }
            if done || count >= MAX_INLINES {
                bi += 1;
            } else {
                bi += 1; // the split block's head has no more statements after the call
            }
        }
    }
}

fn call_of(s: &StmtKind) -> Option<(&Callee, &Vec<Operand>)> {
    match s {
        StmtKind::Assign(_, Rvalue::Call { callee, args, .. }) | StmtKind::Eval(Rvalue::Call { callee, args, .. }) => {
            Some((callee, args))
        }
        _ => None,
    }
}

fn inlinable(b: &Body) -> bool {
    if b.kind != BodyKind::Fn || !b.unsupported.is_empty() || !b.errors.is_empty() || !b.captures.is_empty() {
        return false;
    }
    let n: usize = b.blocks.iter().map(|x| x.stmts.len()).sum();
    if n > MAX_STMTS {
        return false;
    }
    // No loops: a loop needs a preemption check, which the code generator
    // adds only for backward jumps in block order.
    if has_cycle(b) {
        return false;
    }
    for blk in &b.blocks {
        match &blk.term {
            Terminator::Branch(_) => return false,
            Terminator::DeferDispatch { targets, .. } if !targets.is_empty() => return false,
            _ => {}
        }
        for s in &blk.stmts {
            match &s.kind {
                StmtKind::DeferPush(..) | StmtKind::DeferLoad(_) | StmtKind::Spawn { .. } => return false,
                StmtKind::Assign(_, rv) | StmtKind::Eval(rv) => match rv {
                    Rvalue::Aggregate(AggKind::Closure(_), _) => return false,
                    Rvalue::Pure(Op::Unsupported(_), _) => return false,
                    _ => {}
                },
                _ => {}
            }
        }
    }
    true
}

fn has_cycle(b: &Body) -> bool {
    // 0 = unvisited, 1 = on the DFS stack, 2 = done.
    let mut state = vec![0u8; b.blocks.len()];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    state[0] = 1;
    while let Some((n, k)) = stack.pop() {
        let succ = b.blocks[n].term.successors();
        if k < succ.len() {
            stack.push((n, k + 1));
            let m = succ[k].0 as usize;
            match state[m] {
                1 => return true,
                0 => {
                    state[m] = 1;
                    stack.push((m, 0));
                }
                _ => {}
            }
        } else {
            state[n] = 2;
        }
    }
    false
}

/// Replaces call statement `si` of block `bi` with the callee's body.
fn splice(body: &mut Body, bi: usize, si: usize, mut callee: Body, substs: &Substs, args: Vec<Operand>) {
    // Locals: append the callee's, renumbered, with concrete types.
    let base = body.locals.len() as u32;
    for (i, mut d) in callee.locals.drain(..).enumerate() {
        d.ty = d.ty.subst(substs);
        if i == 0 || d.kind == LocalKind::Arg {
            d.kind = LocalKind::Temp;
        }
        body.locals.push(d);
    }
    let ret = Local(base);
    let lmap = |l: Local| Local(l.0 + base);
    // Blocks: the callee's go after the existing ones, then the continuation.
    let bbase = body.blocks.len() as u32;
    let cont = BlockId(bbase + callee.blocks.len() as u32);
    let bmap = |b: BlockId| BlockId(b.0 + bbase);
    let orig = &mut body.blocks[bi];
    let call = orig.stmts[si].clone();
    let tail: Vec<Statement> = orig.stmts.drain(si..).skip(1).collect();
    let cont_term = std::mem::replace(&mut orig.term, Terminator::Goto(BlockId(bbase)));
    let cont_span = orig.term_span;
    // Head: move the arguments into the callee's parameters.
    for (k, a) in args.into_iter().enumerate() {
        orig.stmts.push(Statement {
            kind: StmtKind::Assign(Place::local(Local(base + 1 + k as u32)), Rvalue::Use(a)),
            span: call.span,
        });
    }
    for mut blk in callee.blocks.drain(..) {
        for s in &mut blk.stmts {
            map_stmt(&mut s.kind, &lmap, substs);
        }
        blk.term = match blk.term {
            Terminator::Return => Terminator::Goto(cont),
            Terminator::DeferDispatch { done, .. } => Terminator::Goto(bmap(done)),
            Terminator::Goto(b) => Terminator::Goto(bmap(b)),
            Terminator::SwitchBool { mut cond, t, f } => {
                map_op(&mut cond, &lmap, substs);
                Terminator::SwitchBool { cond, t: bmap(t), f: bmap(f) }
            }
            t @ Terminator::Unreachable => t,
            Terminator::Branch(_) => unreachable!("not inlinable"),
        };
        body.blocks.push(blk);
    }
    // Continuation: deliver the result, then the rest of the original block.
    let mut stmts = Vec::with_capacity(tail.len() + 1);
    match call.kind {
        StmtKind::Assign(p, _) => stmts.push(Statement {
            kind: StmtKind::Assign(p, Rvalue::Use(Operand::Move(Place::local(ret)))),
            span: call.span,
        }),
        _ => stmts.push(Statement { kind: StmtKind::StorageDead(ret), span: call.span }),
    }
    stmts.extend(tail);
    body.blocks.push(BlockData { stmts, term: cont_term, term_span: cont_span });
}

fn map_place(p: &mut Place, lmap: &impl Fn(Local) -> Local) {
    p.local = lmap(p.local);
    for pr in &mut p.proj {
        if let Proj::Index(l) = pr {
            *l = lmap(*l);
        }
    }
}

fn map_substs(v: &mut [(String, Ty)], s: &Substs) {
    for (_, t) in v {
        *t = t.subst(s);
    }
}

fn map_op(o: &mut Operand, lmap: &impl Fn(Local) -> Local, s: &Substs) {
    match o {
        Operand::Copy(p) | Operand::Move(p) => map_place(p, lmap),
        Operand::Const(c) => {
            c.ty = c.ty.subst(s);
            if let ConstKind::Fn(_, v) = &mut c.kind {
                map_substs(v, s);
            }
        }
    }
}

fn map_rvalue(rv: &mut Rvalue, lmap: &impl Fn(Local) -> Local, s: &Substs) {
    match rv {
        Rvalue::Use(o) => map_op(o, lmap, s),
        Rvalue::Coerce(o, t) => {
            map_op(o, lmap, s);
            *t = t.subst(s);
        }
        Rvalue::Ref { place, .. } => map_place(place, lmap),
        Rvalue::Aggregate(_, os) => os.iter_mut().for_each(|o| map_op(o, lmap, s)),
        Rvalue::Call { callee, args, .. } => {
            if let Callee::Fn(_, v) = callee {
                map_substs(v, s);
            }
            args.iter_mut().for_each(|o| map_op(o, lmap, s));
        }
        Rvalue::Pure(op, os) => {
            if let Op::Cast(t) = op {
                *t = t.subst(s);
            }
            os.iter_mut().for_each(|o| map_op(o, lmap, s));
        }
    }
}

fn map_stmt(k: &mut StmtKind, lmap: &impl Fn(Local) -> Local, s: &Substs) {
    match k {
        StmtKind::Assign(p, rv) => {
            map_place(p, lmap);
            map_rvalue(rv, lmap, s);
        }
        StmtKind::Eval(rv) => map_rvalue(rv, lmap, s),
        StmtKind::Read(p) => map_place(p, lmap),
        StmtKind::StorageDead(l) => *l = lmap(*l),
        StmtKind::DeferPush(..) | StmtKind::DeferLoad(_) | StmtKind::Spawn { .. } => unreachable!("not inlinable"),
    }
}
