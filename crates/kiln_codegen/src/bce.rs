//! Bounds-check elimination for loops over a sequence's length (release).
//!
//! Two shapes come out of lowering, and an index in either is proven in
//! range, so `place_addr` skips its check:
//!
//! 1. **Iterator loops** (`for x in v` over a `Vec`, slice, or array): the
//!    loop head computes `n = len(s)` and branches on `i < n`; the body's
//!    first use of `i` is `(*s)[i]`, before `i` is incremented. An index is
//!    safe when, in a block whose only predecessor ends with that branch,
//!    nothing before it writes `i` or calls anything.
//! 2. **Range loops over a length** (`n := v.len(); for i in 0..n { v[i] }`):
//!    the range `r = start..n` has `r.end == len(*b)`, the loop head
//!    branches on `r.start < r.end`, and the body copies `r.start` into a
//!    local before advancing it. Every local that is assigned exactly once,
//!    from such a copy (or from a copy of one), is below `len(*b)`. That
//!    holds for the whole function when the length cannot change: `b` is a
//!    reference assigned once, and every use of it is a shared reborrow, an
//!    element access, or (for `&T`) a copy.
//!
//! Everything else keeps its check.

use crate::Codegen;
use kiln_mir::{
    AggKind, Body, Callee, Local, Op, Operand, Place, Proj, Rvalue, StmtKind, Terminator,
};
use kiln_parser::ast::BinOp;
use kiln_resolve::PrimTy;
use kiln_typeck::ty::Ty;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct Bce {
    /// `(index local, indexed reference local)` safe everywhere.
    pub always: HashSet<(Local, Local)>,
    /// `(block, statement, index local, indexed reference local)` safe at
    /// that statement only.
    pub at: HashSet<(usize, usize, Local, Local)>,
}

impl Bce {
    pub fn is_safe(&self, block: usize, stmt: usize, idx: Local, base: &Place) -> bool {
        // Only `(*r)[i]`: the base is a reference local, dereferenced once.
        if base.proj.len() != 1 || !matches!(base.proj[0], Proj::Deref(_)) {
            return false;
        }
        self.at.contains(&(block, stmt, idx, base.local))
            || self.always.contains(&(idx, base.local))
    }
}

fn is_usize(t: &Ty) -> bool {
    *t == Ty::Prim(PrimTy::Usize)
}

/// Whether `t` is a reference to something with a length that indexing checks
/// (`Vec`, slice, array).
fn ref_to_seq(cg: &Codegen<'_, '_>, t: &Ty) -> Option<bool> {
    match t {
        Ty::Ref(m, x) if matches!(**x, Ty::Slice(_) | Ty::Array(..)) || cg.is_lang(x, "Vec") => {
            Some(*m)
        }
        _ => None,
    }
}

impl Codegen<'_, '_> {
    fn is_len(&self, c: &Callee) -> bool {
        match c {
            Callee::Intrinsic(n) => n == "len",
            Callee::Fn(i, _) => {
                let q = crate::intrinsics::qualified_name(self, *i);
                q == "Vec::len" || q == "slice::len"
            }
            Callee::Value => false,
        }
    }

    pub(crate) fn bounds_facts(&self, body: &Body, substs: &HashMap<String, Ty>) -> Bce {
        let mut out = Bce::default();
        let ty = |l: Local| body.local(l).ty.subst(substs);
        // Single assignments (whole-local writes) and their rvalues.
        let mut writes: HashMap<Local, usize> = HashMap::new();
        let mut def: HashMap<Local, &Rvalue> = HashMap::new();
        let mut mut_borrowed: HashSet<Local> = HashSet::new();
        for blk in &body.blocks {
            for s in &blk.stmts {
                if let StmtKind::Assign(p, rv) = &s.kind {
                    if let Rvalue::Ref {
                        mutable: true,
                        place,
                    } = rv
                    {
                        mut_borrowed.insert(place.local);
                    }
                    // A write through a reference changes the referent, not the local.
                    if matches!(p.proj.first(), Some(Proj::Deref(_))) {
                        continue;
                    }
                    *writes.entry(p.local).or_default() += 1;
                    if p.proj.is_empty() {
                        def.insert(p.local, rv);
                    } else {
                        // A partial write: not a single definition.
                        *writes.entry(p.local).or_default() += 1;
                    }
                }
            }
        }
        let single = |l: Local| writes.get(&l) == Some(&1) && !mut_borrowed.contains(&l);
        let preds = predecessors(body);

        // ---- shape 1: iterator loops ----
        for blk in &body.blocks {
            let Terminator::SwitchBool {
                cond: Operand::Copy(c),
                t,
                ..
            } = &blk.term
            else {
                continue;
            };
            if !c.proj.is_empty() || preds.get(&t.0).map(|v| v.len()) != Some(1) {
                continue;
            }
            // `c = i < n` and `n = len(s)` in this block, no call between them.
            let mut guard = None;
            let mut len_of: HashMap<Local, Local> = HashMap::new();
            for s in &blk.stmts {
                match &s.kind {
                    StmtKind::Assign(p, Rvalue::Call { callee, args, .. }) if p.proj.is_empty() => {
                        len_of.clear();
                        if self.is_len(callee) {
                            if let [Operand::Copy(a)] = args.as_slice() {
                                if a.proj.is_empty() && ref_to_seq(self, &ty(a.local)).is_some() {
                                    len_of.insert(p.local, a.local);
                                }
                            }
                        }
                    }
                    StmtKind::Assign(p, Rvalue::Pure(Op::Bin(BinOp::Lt), ops))
                        if p.local == c.local =>
                    {
                        if let [Operand::Copy(i), Operand::Copy(n)] = ops.as_slice() {
                            if i.proj.is_empty() && n.proj.is_empty() && is_usize(&ty(i.local)) {
                                guard = len_of.get(&n.local).map(|s| (i.local, *s));
                            }
                        }
                    }
                    StmtKind::Eval(Rvalue::Call { .. }) => len_of.clear(),
                    StmtKind::Assign(p, _) => {
                        len_of.retain(|n, sq| *n != p.local && *sq != p.local)
                    }
                    _ => {}
                }
            }
            let Some((i, seq)) = guard else { continue };
            let tb = t.0 as usize;
            for (si, s) in body.blocks[tb].stmts.iter().enumerate() {
                let (place_ok, stop) = match &s.kind {
                    StmtKind::Assign(p, rv) => {
                        let stop =
                            p.local == i || p.local == seq || matches!(rv, Rvalue::Call { .. });
                        (true, stop)
                    }
                    StmtKind::Eval(_) => (false, true),
                    _ => (false, false),
                };
                if place_ok {
                    out.at.insert((tb, si, i, seq));
                }
                if stop {
                    break;
                }
            }
        }

        // ---- shape 2: range loops over a length ----
        // Root of a reference local: through single `copy` / `&*` chains.
        let root = |mut l: Local| {
            for _ in 0..16 {
                if !single(l) {
                    break;
                }
                match def.get(&l) {
                    Some(Rvalue::Use(Operand::Copy(p))) if p.proj.is_empty() => l = p.local,
                    Some(Rvalue::Ref {
                        mutable: false,
                        place,
                    }) if place.proj.len() == 1 && matches!(place.proj[0], Proj::Deref(_)) => {
                        l = place.local
                    }
                    _ => break,
                }
            }
            l
        };
        // Length-invariant reference roots.
        let invariant = |r: Local| -> bool {
            let Some(m) = ref_to_seq(self, &ty(r)) else {
                return false;
            };
            let is_arg = (1..=body.arg_count).contains(&(r.0 as usize));
            if (!is_arg || writes.contains_key(&r)) && !single(r) {
                return false;
            }
            if !m {
                return true; // `&T`: nothing can change the length
            }
            // `&mut T`: only element accesses and shared reborrows.
            let ok_place = |p: &Place| {
                p.local != r
                    || (p.proj.len() >= 2
                        && matches!(p.proj[0], Proj::Deref(_))
                        && matches!(p.proj[1], Proj::Index(_)))
            };
            for blk in &body.blocks {
                for s in &blk.stmts {
                    let (dest, rv): (Option<&Place>, Option<&Rvalue>) = match &s.kind {
                        StmtKind::Assign(p, rv) => (Some(p), Some(rv)),
                        StmtKind::Eval(rv) => (None, Some(rv)),
                        _ => (None, None),
                    };
                    if dest.is_some_and(|p| p.local == r && !ok_place(p)) {
                        return false;
                    }
                    let ops: Vec<&Operand> = match (&s.kind, rv) {
                        (StmtKind::Spawn { operands, .. }, _) => operands.iter().collect(),
                        (_, Some(Rvalue::Ref { mutable, place })) if place.local == r => {
                            let shared_whole = !mutable && place.proj.len() == 1;
                            if !shared_whole && !ok_place(place) {
                                return false;
                            }
                            vec![]
                        }
                        (_, Some(rv)) => rv.operands(),
                        _ => vec![],
                    };
                    for o in ops {
                        if let Some(p) = o.place() {
                            if p.local == r && !ok_place(p) {
                                return false;
                            }
                        }
                    }
                }
            }
            true
        };
        // `n` locals that hold `len(*root)` for an invariant root.
        let len_root = |n: Local| -> Option<Local> {
            let mut n = n;
            for _ in 0..8 {
                if !single(n) {
                    return None;
                }
                match def.get(&n)? {
                    Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => n = p.local,
                    Rvalue::Call { callee, args, .. } if self.is_len(callee) => {
                        let [Operand::Copy(a)] = args.as_slice() else {
                            return None;
                        };
                        if !a.proj.is_empty() {
                            return None;
                        }
                        let r = root(a.local);
                        return invariant(r).then_some(r);
                    }
                    _ => return None,
                }
            }
            None
        };
        // Range locals `r` with `r.end == len(*root)` and only `r.start` written later.
        let range_root = |r: Local| -> Option<Local> {
            let mut whole = 0;
            for blk in &body.blocks {
                for s in &blk.stmts {
                    if let StmtKind::Assign(p, _) = &s.kind {
                        if p.local == r {
                            match p.proj.as_slice() {
                                [] => whole += 1,
                                [Proj::Field(f)] if f == "start" => {}
                                _ => return None,
                            }
                        }
                    }
                }
            }
            if whole != 1 || mut_borrowed.contains(&r) {
                return None;
            }
            let mut rv = *def.get(&r)?;
            for _ in 0..4 {
                match rv {
                    Rvalue::Use(Operand::Move(p) | Operand::Copy(p))
                        if p.proj.is_empty() && single(p.local) =>
                    {
                        rv = def.get(&p.local)?;
                    }
                    Rvalue::Aggregate(
                        AggKind::Range {
                            has_end: true,
                            inclusive: false,
                            ..
                        },
                        ops,
                    ) => {
                        let Operand::Copy(e) = ops.last()? else {
                            return None;
                        };
                        if !e.proj.is_empty() {
                            return None;
                        }
                        return len_root(e.local);
                    }
                    _ => return None,
                }
            }
            None
        };
        let mut safe: HashMap<Local, Local> = HashMap::new(); // index local -> root
        for blk in &body.blocks {
            let Terminator::SwitchBool {
                cond: Operand::Copy(c),
                t,
                ..
            } = &blk.term
            else {
                continue;
            };
            if !c.proj.is_empty() || preds.get(&t.0).map(|v| v.len()) != Some(1) {
                continue;
            }
            let mut range = None;
            for s in &blk.stmts {
                if let StmtKind::Assign(p, Rvalue::Pure(Op::Bin(BinOp::Lt), ops)) = &s.kind {
                    if p.local == c.local {
                        if let [Operand::Copy(a), Operand::Copy(b)] = ops.as_slice() {
                            let fa = matches!(a.proj.as_slice(), [Proj::Field(f)] if f == "start");
                            let fb = matches!(b.proj.as_slice(), [Proj::Field(f)] if f == "end");
                            range = (fa && fb && a.local == b.local).then_some(a.local);
                        }
                    }
                }
            }
            let Some(r) = range else { continue };
            let Some(rt) = range_root(r) else { continue };
            // In the body: `x = copy r.start` before `r` is written.
            for s in &body.blocks[t.0 as usize].stmts {
                if let StmtKind::Assign(p, rv) = &s.kind {
                    if p.local == r {
                        break;
                    }
                    if let Rvalue::Use(Operand::Copy(q)) = rv {
                        let from_start = q.local == r
                            && matches!(q.proj.as_slice(), [Proj::Field(f)] if f == "start");
                        if from_start
                            && p.proj.is_empty()
                            && single(p.local)
                            && is_usize(&ty(p.local))
                        {
                            safe.insert(p.local, rt);
                        }
                    }
                }
            }
        }
        // Copies of safe locals are safe.
        loop {
            let mut grew = false;
            for (l, rv) in &def {
                if safe.contains_key(l) || !single(*l) {
                    continue;
                }
                if let Rvalue::Use(Operand::Copy(p)) = rv {
                    if p.proj.is_empty() {
                        if let Some(rt) = safe.get(&p.local).copied() {
                            safe.insert(*l, rt);
                            grew = true;
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        // Indexed reference locals whose root is the range's root.
        let mut bases: HashSet<Local> = HashSet::new();
        for blk in &body.blocks {
            for s in &blk.stmts {
                let mut places: Vec<&Place> = vec![];
                match &s.kind {
                    StmtKind::Assign(p, rv) => {
                        places.push(p);
                        if let Rvalue::Ref { place, .. } = rv {
                            places.push(place);
                        }
                        places.extend(rv.operands().into_iter().filter_map(|o| o.place()));
                    }
                    StmtKind::Eval(rv) => {
                        places.extend(rv.operands().into_iter().filter_map(|o| o.place()))
                    }
                    _ => {}
                }
                for p in places {
                    if p.proj.len() >= 2 && matches!(p.proj[0], Proj::Deref(_)) {
                        if let Proj::Index(_) = p.proj[1] {
                            bases.insert(p.local);
                        }
                    }
                }
            }
        }
        for b in bases {
            let rb = root(b);
            for (i, rt) in &safe {
                if *rt == rb {
                    out.always.insert((*i, b));
                }
            }
        }
        out
    }
}

fn predecessors(body: &Body) -> HashMap<u32, Vec<usize>> {
    let mut m: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, b) in body.blocks.iter().enumerate() {
        for s in b.term.successors() {
            m.entry(s.0).or_default().push(i);
        }
    }
    m
}
