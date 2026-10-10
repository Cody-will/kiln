//! Borrow checking of one MIR body.
//!
//! Three analyses run over the control-flow graph:
//!
//! 1. **Initialization and moves** (forward, may-analysis): which places may
//!    be moved-out or uninitialized at each point. Using such a place is an
//!    error ("use of moved value"), with a note pointing at the move.
//! 2. **Liveness** (backward): which locals may still be read later.
//! 3. **Loans**: every `&`/`&mut` creates a loan; a loan is *held* by the
//!    locals its reference flows into (through copies, aggregates, and call
//!    results according to signature lifetimes). A loan is *live* at a point
//!    when some local holding it is live there, which gives non-lexical
//!    lifetimes: a borrow ends at its last use, not at the end of its scope.
//!    Accesses that conflict with a live loan are errors, as is ending a
//!    variable's storage while a loan on it is live.
//!
//! The return place is checked for references to locals and for borrows the
//! signature does not allow; `go` and scoped spawns are checked for `'static`,
//! `Send`, and `Sync`.

use kiln_lexer::{Diagnostic, Span};
use kiln_mir::*;
use kiln_resolve::DefId;
use kiln_typeck::ty::Ty;
use kiln_typeck::Typed;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

type Loc = (usize, usize);

#[derive(Debug, Clone)]
struct Loan {
    /// Borrowed place (`None` for the incoming borrow of a reference parameter).
    place: Option<Place>,
    mutable: bool,
    span: Span,
    /// For parameter loans: the parameter index (`self` first).
    param: Option<usize>,
}

/// Where a place became moved or was never initialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MoveSite {
    /// `None`: never initialized.
    span: Option<(usize, usize)>,
}

type MoveState = BTreeMap<(Local, Vec<Proj>), MoveSite>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    SharedBorrow,
    MutBorrow,
    Write,
    Move,
    Dead,
}

pub(crate) struct Ck<'b, 't, 'r, 'a> {
    b: &'b Body,
    typed: &'t mut Typed<'r, 'a>,
    bounds: Vec<(String, DefId)>,
    pub(crate) errors: Vec<Diagnostic>,
    seen: HashSet<(Span, String)>,
    reachable: Vec<bool>,
    loans: Vec<Loan>,
    loan_at: BTreeMap<Loc, usize>,
    holds: Vec<BTreeSet<usize>>,
    /// Flow-sensitive loan sets at the entry of each block.
    hin: Vec<Option<Vec<BTreeSet<usize>>>>,
    /// live_out[block][stmt] for every statement, and live-out of the terminator at index `len`.
    live: Vec<Vec<BTreeSet<Local>>>,
    /// Closures handed to `Scope::spawn`: they keep their loans until the body returns.
    pinned: BTreeSet<Local>,
    may_hold_cache: Vec<bool>,
    drop_cache: Vec<bool>,
}

impl<'b, 't, 'r, 'a> Ck<'b, 't, 'r, 'a> {
    pub(crate) fn new(
        b: &'b Body,
        typed: &'t mut Typed<'r, 'a>,
        bounds: Vec<(String, DefId)>,
    ) -> Self {
        Ck {
            b,
            typed,
            bounds,
            errors: vec![],
            seen: HashSet::new(),
            reachable: vec![],
            loans: vec![],
            loan_at: BTreeMap::new(),
            holds: vec![],
            hin: vec![],
            live: vec![],
            pinned: BTreeSet::new(),
            may_hold_cache: vec![],
            drop_cache: vec![],
        }
    }

    pub(crate) fn run(&mut self) {
        for (msg, span) in self.b.errors.clone() {
            self.report(Diagnostic::error(msg, span).with_help(
                "deferred calls run when the function returns; declare the variable at the top level of the function body",
            ));
        }
        self.reachable = self.compute_reachable();
        let n = self.b.locals.len();
        self.may_hold_cache = (0..n)
            .map(|i| self.may_hold(&self.b.locals[i].ty.clone()))
            .collect();
        self.drop_cache = (0..n)
            .map(|i| {
                let t = self.b.locals[i].ty.clone();
                self.typed.has_drop_impl(&t)
            })
            .collect();
        self.pinned = self
            .b
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter_map(|st| match &st.kind {
                StmtKind::Spawn {
                    kind: SpawnKind::Scoped,
                    operands,
                    ..
                } => Some(operands.iter().filter_map(|o| o.place().map(|p| p.local))),
                _ => None,
            })
            .flatten()
            .collect();
        self.compute_loans();
        self.compute_liveness();
        self.check_moves();
        self.check_loans();
        self.check_return();
        self.check_spawns();
    }

    fn report(&mut self, d: Diagnostic) {
        if self.seen.insert((d.span, d.message.clone())) {
            self.errors.push(d);
        }
    }

    // ----- helpers -------------------------------------------------------------------

    fn compute_reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.b.blocks.len()];
        let mut q = VecDeque::from([0usize]);
        seen[0] = true;
        while let Some(b) = q.pop_front() {
            for s in self.b.blocks[b].term.successors() {
                if !seen[s.0 as usize] {
                    seen[s.0 as usize] = true;
                    q.push_back(s.0 as usize);
                }
            }
        }
        seen
    }

    /// Whether values of type `t` can contain borrows.
    fn may_hold(&mut self, t: &Ty) -> bool {
        match t {
            Ty::Ref(..) | Ty::Param(_) | Ty::Fn(..) | Ty::Dyn(_) => true,
            Ty::Adt(d, args) => {
                self.typed.adt_has_lifetimes(*d) || args.iter().any(|a| self.may_hold(a))
            }
            Ty::Tuple(ts) => ts.iter().any(|x| self.may_hold(x)),
            Ty::Array(x, _) | Ty::Slice(x) | Ty::ChanSend(x) | Ty::ChanRecv(x) => self.may_hold(x),
            _ => false,
        }
    }

    fn ty_of(&self, l: Local) -> &Ty {
        &self.b.local(l).ty
    }

    fn stmt(&self, loc: Loc) -> &Statement {
        &self.b.blocks[loc.0].stmts[loc.1]
    }

    fn succ(&self, b: usize) -> Vec<usize> {
        self.b.blocks[b]
            .term
            .successors()
            .into_iter()
            .map(|x| x.0 as usize)
            .collect()
    }

    // ----- loans ----------------------------------------------------------------------

    fn compute_loans(&mut self) {
        let n = self.b.locals.len();
        self.holds = vec![BTreeSet::new(); n];
        for i in 1..=self.b.arg_count {
            if self.may_hold_cache[i] && self.b.kind == BodyKind::Fn {
                let id = self.loans.len();
                self.loans.push(Loan {
                    place: None,
                    mutable: false,
                    span: self.b.locals[i].span,
                    param: Some(i - 1),
                });
                self.holds[i].insert(id);
            }
        }
        for (bi, bb) in self.b.blocks.iter().enumerate() {
            if !self.reachable[bi] {
                continue;
            }
            for (si, s) in bb.stmts.iter().enumerate() {
                if let StmtKind::Assign(_, Rvalue::Ref { mutable, place }) = &s.kind {
                    self.loan_at.insert((bi, si), self.loans.len());
                    self.loans.push(Loan {
                        place: Some(place.clone()),
                        mutable: *mutable,
                        span: s.span,
                        param: None,
                    });
                }
            }
        }
        // Flow-sensitive: which loans each local may hold at each point.
        // Assigning a whole local replaces its loans (so a reassigned reference
        // variable no longer keeps its old borrow alive).
        let nb = self.b.blocks.len();
        let entry = self.holds.clone();
        let mut hin: Vec<Option<Vec<BTreeSet<usize>>>> = vec![None; nb];
        hin[0] = Some(entry);
        let mut work = VecDeque::from([0usize]);
        while let Some(bi) = work.pop_front() {
            let mut st = hin[bi].clone().unwrap();
            for si in 0..self.b.blocks[bi].stmts.len() {
                self.holds_transfer(&mut st, (bi, si));
            }
            for sb in self.succ(bi) {
                let changed = match &mut hin[sb] {
                    None => {
                        hin[sb] = Some(st.clone());
                        true
                    }
                    Some(e) => {
                        let mut ch = false;
                        for (a, b) in e.iter_mut().zip(&st) {
                            let before = a.len();
                            a.extend(b.iter().copied());
                            ch |= a.len() != before;
                        }
                        ch
                    }
                };
                if changed && !work.contains(&sb) {
                    work.push_back(sb);
                }
            }
        }
        // The flow-insensitive union is used for "borrow later used here" notes.
        for st in hin.iter().flatten() {
            for (a, b) in self.holds.iter_mut().zip(st) {
                a.extend(b.iter().copied());
            }
        }
        for (bi, h) in hin.iter().enumerate() {
            if let Some(h) = h {
                let mut st = h.clone();
                for si in 0..self.b.blocks[bi].stmts.len() {
                    self.holds_transfer(&mut st, (bi, si));
                    for (a, b) in self.holds.iter_mut().zip(&st) {
                        a.extend(b.iter().copied());
                    }
                }
            }
        }
        self.hin = hin;
    }

    fn op_holds_in(st: &[BTreeSet<usize>], o: &Operand) -> BTreeSet<usize> {
        o.place()
            .map(|p| st[p.local.0 as usize].clone())
            .unwrap_or_default()
    }

    /// Applies statement `loc` to the per-local loan sets.
    fn holds_transfer(&self, st: &mut [BTreeSet<usize>], loc: Loc) {
        match &self.stmt(loc).kind {
            StmtKind::Assign(dest, rv) => {
                let root = dest.local.0 as usize;
                if !self.may_hold_cache[root] {
                    return;
                }
                let add: BTreeSet<usize> = match rv {
                    Rvalue::Use(o) => Self::op_holds_in(st, o),
                    Rvalue::Ref { place, .. } => {
                        let mut s = BTreeSet::from([self.loan_at[&loc]]);
                        if place.through_ref() {
                            s.extend(st[place.local.0 as usize].iter().copied());
                        }
                        s
                    }
                    Rvalue::Aggregate(_, ops) => {
                        ops.iter().flat_map(|o| Self::op_holds_in(st, o)).collect()
                    }
                    Rvalue::Call { args, flows, .. } => flows
                        .iter()
                        .filter_map(|i| args.get(*i))
                        .flat_map(|o| Self::op_holds_in(st, o))
                        .collect(),
                    Rvalue::Pure(..) => BTreeSet::new(),
                    Rvalue::Coerce(o, _) => Self::op_holds_in(st, o),
                };
                if dest.proj.is_empty() {
                    st[root] = add;
                } else {
                    st[root].extend(add);
                }
            }
            StmtKind::StorageDead(l) if !self.pinned.contains(l) => st[l.0 as usize].clear(),
            _ => {}
        }
    }

    /// Loan sets before statement `si` of block `bi`.
    fn holds_before(&self, bi: usize, si: usize) -> Vec<BTreeSet<usize>> {
        let mut st = self.hin[bi]
            .clone()
            .unwrap_or_else(|| vec![BTreeSet::new(); self.b.locals.len()]);
        for i in 0..si {
            self.holds_transfer(&mut st, (bi, i));
        }
        st
    }

    /// Locals holding loan `l`.
    fn holders(&self, l: usize) -> Vec<Local> {
        (0..self.holds.len())
            .filter(|i| self.holds[*i].contains(&l))
            .map(|i| Local(i as u32))
            .collect()
    }

    // ----- liveness ---------------------------------------------------------------------

    fn uses_defs(&self, s: &Statement) -> (Vec<Local>, Option<Local>) {
        let mut uses = vec![];
        let mut def = None;
        let rv_uses = |rv: &Rvalue, uses: &mut Vec<Local>| {
            for o in rv.operands() {
                if let Some(p) = o.place() {
                    uses.push(p.local);
                }
            }
            if let Rvalue::Ref { place, .. } = rv {
                uses.push(place.local);
            }
        };
        match &s.kind {
            StmtKind::Assign(dest, rv) => {
                rv_uses(rv, &mut uses);
                if dest.proj.is_empty() {
                    def = Some(dest.local);
                } else {
                    uses.push(dest.local);
                }
            }
            StmtKind::Read(p) => uses.push(p.local),
            // The saved arguments stay live from the `defer` to the deferred call.
            StmtKind::DeferPush(..) | StmtKind::DeferLoad(_) => {}
            StmtKind::Eval(rv) => rv_uses(rv, &mut uses),
            StmtKind::StorageDead(l) => {
                if self.drop_cache[l.0 as usize] {
                    uses.push(*l);
                }
            }
            StmtKind::Spawn { operands, .. } => {
                uses.extend(operands.iter().filter_map(|o| o.place().map(|p| p.local)))
            }
        }
        (uses, def)
    }

    fn compute_liveness(&mut self) {
        let nb = self.b.blocks.len();
        let mut live_in: Vec<BTreeSet<Local>> = vec![BTreeSet::new(); nb];
        loop {
            let mut changed = false;
            for bi in (0..nb).rev() {
                if !self.reachable[bi] {
                    continue;
                }
                let mut live = self.block_live_out(bi, &live_in);
                for s in self.b.blocks[bi].stmts.iter().rev() {
                    let (uses, def) = self.uses_defs(s);
                    if let Some(d) = def {
                        live.remove(&d);
                    }
                    live.extend(uses);
                }
                if live != live_in[bi] {
                    live_in[bi] = live;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.live = (0..nb)
            .map(|bi| {
                let n = self.b.blocks[bi].stmts.len();
                let mut out = vec![BTreeSet::new(); n + 1];
                let mut live = self.block_live_out(bi, &live_in);
                out[n] = live.clone();
                for (si, s) in self.b.blocks[bi].stmts.iter().enumerate().rev() {
                    out[si] = live.clone();
                    let (uses, def) = self.uses_defs(s);
                    if let Some(d) = def {
                        live.remove(&d);
                    }
                    live.extend(uses);
                }
                out
            })
            .collect();
    }

    fn block_live_out(&self, bi: usize, live_in: &[BTreeSet<Local>]) -> BTreeSet<Local> {
        let mut live: BTreeSet<Local> = BTreeSet::new();
        for s in self.succ(bi) {
            live.extend(live_in[s].iter().copied());
        }
        if matches!(self.b.blocks[bi].term, Terminator::Return) {
            live.insert(Local(0));
            // Closures handed to `Scope::spawn` run until the scope ends, so
            // the loans they hold last until this body returns.
            live.extend(self.pinned.iter().copied());
        }
        live
    }

    /// The first statement after `loc` that uses one of `locals`, as `(span, via drop of)`.
    fn next_use(&self, loc: Loc, locals: &[Local]) -> Option<(Span, Option<Local>)> {
        let mut seen = HashSet::new();
        let mut q = VecDeque::from([(loc.0, loc.1 + 1)]);
        while let Some((b, start)) = q.pop_front() {
            let bb = &self.b.blocks[b];
            for s in &bb.stmts[start.min(bb.stmts.len())..] {
                let (uses, _) = self.uses_defs(s);
                if let Some(l) = uses.iter().find(|u| locals.contains(u)) {
                    let via_drop = matches!(s.kind, StmtKind::StorageDead(_)).then_some(*l);
                    return Some((s.span, via_drop));
                }
            }
            if matches!(bb.term, Terminator::Return) && locals.contains(&Local(0)) {
                return Some((bb.term_span, None));
            }
            for s in self.succ(b) {
                if seen.insert(s) {
                    q.push_back((s, 0));
                }
            }
        }
        None
    }

    // ----- moves -------------------------------------------------------------------------

    fn entry_state(&self) -> MoveState {
        let mut st = MoveState::new();
        for (i, l) in self.b.locals.iter().enumerate() {
            if matches!(
                l.kind,
                LocalKind::User | LocalKind::Temp | LocalKind::Return
            ) {
                st.insert((Local(i as u32), vec![]), MoveSite { span: None });
            }
        }
        st
    }

    fn check_moves(&mut self) {
        let nb = self.b.blocks.len();
        let mut entry: Vec<Option<MoveState>> = vec![None; nb];
        entry[0] = Some(self.entry_state());
        let mut work: VecDeque<usize> = VecDeque::from([0]);
        while let Some(bi) = work.pop_front() {
            let mut st = entry[bi].clone().unwrap();
            for si in 0..self.b.blocks[bi].stmts.len() {
                self.move_transfer(&mut st, (bi, si), false);
            }
            for s in self.succ(bi) {
                let changed = match &mut entry[s] {
                    None => {
                        entry[s] = Some(st.clone());
                        true
                    }
                    Some(e) => {
                        let before = e.len();
                        for (k, v) in &st {
                            e.entry(k.clone()).or_insert(*v);
                        }
                        e.len() != before
                    }
                };
                if changed && !work.contains(&s) {
                    work.push_back(s);
                }
            }
        }
        for (bi, e) in entry.into_iter().enumerate() {
            let Some(mut st) = e else { continue };
            for si in 0..self.b.blocks[bi].stmts.len() {
                self.move_transfer(&mut st, (bi, si), true);
            }
        }
    }

    fn move_transfer(&mut self, st: &mut MoveState, loc: Loc, report: bool) {
        let s = self.stmt(loc).clone();
        let mut uses: Vec<(&Operand, bool)> = vec![];
        let mut borrow: Option<&Place> = None;
        match &s.kind {
            StmtKind::Assign(_, rv) | StmtKind::Eval(rv) => {
                for o in rv.operands() {
                    uses.push((o, false));
                }
                if let Rvalue::Ref { place, .. } = rv {
                    borrow = Some(place);
                }
            }
            StmtKind::Spawn { operands, .. } => {
                for o in operands {
                    uses.push((o, true));
                }
            }
            StmtKind::Read(p) => {
                if report {
                    self.check_init(st, p, false, s.span, loc);
                }
            }
            StmtKind::StorageDead(_) | StmtKind::DeferPush(..) => {}
            StmtKind::DeferLoad(l) => {
                // The deferred call's block runs only after its `defer`, so the
                // saved arguments are initialized there.
                st.retain(|(x, _), _| x != l);
            }
        }
        if report {
            for (o, _) in &uses {
                if let Some(p) = o.place() {
                    self.check_init(st, p, false, s.span, loc);
                }
            }
            if let Some(p) = borrow {
                self.check_init(st, p, true, s.span, loc);
            }
        }
        for (o, _) in &uses {
            if let Operand::Move(p) = o {
                self.apply_move(st, p, s.span, loc, report);
            }
        }
        if let StmtKind::Assign(dest, _) = &s.kind {
            let decl = self.b.local(dest.local);
            if report
                && dest.proj.is_empty()
                && decl.kind == LocalKind::User
                && !decl.mutable
                && !st.contains_key(&(dest.local, vec![]))
            {
                let name = decl.name.clone().unwrap_or_default();
                self.report(
                    Diagnostic::error(format!("cannot assign twice to immutable variable `{name}`"), s.span)
                        .with_note("first assigned here", decl.span)
                        .with_help(format!("declare it with `let mut {name}` (or `{name} mut := ...`) to allow reassignment")),
                );
            }
            if !dest.through_ref() {
                st.retain(|(l, path), _| !(*l == dest.local && path.starts_with(&dest.proj)));
            }
        }
    }

    fn check_init(&mut self, st: &MoveState, p: &Place, is_borrow: bool, span: Span, loc: Loc) {
        // Only the part of the path up to the first deref of a reference is owned storage.
        let owned_len = p
            .proj
            .iter()
            .position(|x| {
                matches!(
                    x,
                    Proj::Deref(DerefKindKey::Shared | DerefKindKey::Mut | DerefKindKey::Ptr)
                )
            })
            .unwrap_or(p.proj.len());
        let path = &p.proj[..owned_len];
        let whole = Place {
            local: p.local,
            proj: path.to_vec(),
        };
        for ((l, mp), site) in st.iter() {
            if *l != p.local {
                continue;
            }
            let ancestor = path.starts_with(mp);
            let partial = !ancestor && mp.starts_with(path);
            if !ancestor && !partial {
                continue;
            }
            let decl = self.b.local(p.local);
            if decl.kind == LocalKind::Temp || decl.kind == LocalKind::Return {
                return;
            }
            let name = self.b.describe(&whole);
            let moved_name = self.b.describe(&Place {
                local: *l,
                proj: mp.clone(),
            });
            let ty = self.typed.fmt_ty(&decl.ty);
            let d = match site.span {
                None => Diagnostic::error(
                    format!(
                        "used binding `{}` isn't initialized",
                        decl.name.clone().unwrap_or_default()
                    ),
                    span,
                )
                .with_note("binding declared here but left uninitialized", decl.span),
                Some((ms, me)) => {
                    let what = match (partial, is_borrow) {
                        (true, true) => "borrow of partially moved value",
                        (true, false) => "use of partially moved value",
                        (false, true) => "borrow of moved value",
                        (false, false) => "use of moved value",
                    };
                    let move_span = Span::new(ms, me);
                    let in_loop =
                        ms >= span.start && self.stmt(loc).span != move_span || move_span == span;
                    let note = if partial {
                        format!("value partially moved here (`{moved_name}`)")
                    } else if in_loop {
                        "value moved here, in a previous iteration of the loop".to_string()
                    } else {
                        "value moved here".to_string()
                    };
                    Diagnostic::error(format!("{what}: `{name}`"), span)
                        .with_note(note, move_span)
                        .with_plain_note(format!("move occurs because `{moved_name}` has type `{ty}`, which does not implement the `Copy` trait"))
                        .with_help(format!("borrow it with `&{moved_name}` or call `.clone()` before moving it"))
                }
            };
            self.report(d);
            return;
        }
    }

    fn apply_move(&mut self, st: &mut MoveState, p: &Place, span: Span, _loc: Loc, report: bool) {
        let decl = self.b.local(p.local).clone();
        if let Some(i) = p.proj.iter().position(|x| {
            matches!(
                x,
                Proj::Deref(DerefKindKey::Shared | DerefKindKey::Mut | DerefKindKey::Ptr)
            )
        }) {
            if report {
                let kind = match p.proj[i] {
                    Proj::Deref(DerefKindKey::Mut) => "a mutable reference",
                    Proj::Deref(DerefKindKey::Ptr) => "a raw pointer",
                    _ => "a shared reference",
                };
                let name = self.b.describe(p);
                self.report(
                    Diagnostic::error(
                        format!("cannot move out of `{name}`, which is behind {kind}"),
                        span,
                    )
                    .with_help("borrow the value instead, or call `.clone()` to get an owned copy"),
                );
            }
            return;
        }
        if p.proj.iter().any(|x| matches!(x, Proj::Index(_))) {
            if report {
                let name = self.b.describe(p);
                self.report(
                    Diagnostic::error(format!("cannot move out of index of `{name}`"), span)
                        .with_help("borrow the element with `&`, call `.clone()`, or remove it from the collection"),
                );
            }
            return;
        }
        if !p.proj.is_empty() && self.drop_cache[p.local.0 as usize] {
            if report {
                let t = self.typed.fmt_ty(&decl.ty);
                self.report(
                    Diagnostic::error(
                        format!("cannot move out of type `{t}`, which implements the `Drop` trait"),
                        span,
                    )
                    .with_help("fields of a type with a destructor can only be borrowed or cloned"),
                );
            }
            return;
        }
        st.entry((p.local, p.proj.clone())).or_insert(MoveSite {
            span: Some((span.start, span.end)),
        });
    }

    // ----- borrow conflicts ---------------------------------------------------------------

    fn accesses(s: &Statement) -> Vec<(Place, Access)> {
        let mut out = vec![];
        let ops = |rv: &Rvalue, out: &mut Vec<(Place, Access)>| {
            for o in rv.operands() {
                match o {
                    Operand::Copy(p) => out.push((p.clone(), Access::Read)),
                    Operand::Move(p) => out.push((p.clone(), Access::Move)),
                    Operand::Const(_) => {}
                }
            }
            if let Rvalue::Ref { mutable, place } = rv {
                out.push((
                    place.clone(),
                    if *mutable {
                        Access::MutBorrow
                    } else {
                        Access::SharedBorrow
                    },
                ));
            }
        };
        match &s.kind {
            StmtKind::Assign(dest, rv) => {
                ops(rv, &mut out);
                out.push((dest.clone(), Access::Write));
            }
            StmtKind::Eval(rv) => ops(rv, &mut out),
            StmtKind::Read(p) => out.push((p.clone(), Access::Read)),
            StmtKind::DeferPush(..) | StmtKind::DeferLoad(_) => {}
            StmtKind::StorageDead(l) => out.push((Place::local(*l), Access::Dead)),
            StmtKind::Spawn { operands, .. } => {
                for o in operands {
                    match o {
                        Operand::Copy(p) => out.push((p.clone(), Access::Read)),
                        Operand::Move(p) => out.push((p.clone(), Access::Move)),
                        Operand::Const(_) => {}
                    }
                }
            }
        }
        out
    }

    fn check_loans(&mut self) {
        for bi in 0..self.b.blocks.len() {
            if !self.reachable[bi] {
                continue;
            }
            let mut st = self.holds_before(bi, 0);
            for si in 0..self.b.blocks[bi].stmts.len() {
                let s = self.b.blocks[bi].stmts[si].clone();
                let created = self.loan_at.get(&(bi, si)).copied();
                let live_after = self.live[bi][si].clone();
                self.holds_transfer(&mut st, (bi, si));
                for (p, acc) in Self::accesses(&s) {
                    let mut live_loans = BTreeSet::new();
                    for l in &live_after {
                        if acc == Access::Dead && *l == Local(0) {
                            continue; // reported by the return check
                        }
                        live_loans.extend(st[l.0 as usize].iter().copied());
                    }
                    for li in live_loans {
                        if Some(li) == created {
                            continue;
                        }
                        let loan = self.loans[li].clone();
                        let Some(lp) = &loan.place else { continue };
                        let conflict = match acc {
                            Access::Dead => lp.local == p.local && !lp.through_ref(),
                            Access::Read | Access::SharedBorrow => loan.mutable && lp.overlaps(&p),
                            Access::Write | Access::MutBorrow | Access::Move => lp.overlaps(&p),
                        };
                        // Writing through the reference that holds the loan is how `&mut` is used.
                        if conflict && !(acc == Access::Write && p.through_ref()) {
                            self.loan_conflict(&loan, li, &p, acc, (bi, si), s.span);
                            break;
                        }
                    }
                }
            }
        }
    }

    fn loan_conflict(
        &mut self,
        loan: &Loan,
        li: usize,
        p: &Place,
        acc: Access,
        loc: Loc,
        span: Span,
    ) {
        let holders: Vec<Local> = self
            .holders(li)
            .into_iter()
            .filter(|l| acc != Access::Dead || *l != Local(0))
            .collect();
        let later = self.next_use(loc, &holders);
        let lp = loan.place.clone().unwrap();
        let name = self.b.describe(&lp);
        let later_note = |d: Diagnostic, what: &str| match later {
            Some((sp, Some(dropped))) => d.with_note(
                format!(
                    "{what} might be used here, when `{}` is dropped",
                    self.b.describe(&Place::local(dropped))
                ),
                sp,
            ),
            Some((sp, None)) => d.with_note(format!("{what} later used here"), sp),
            None => d,
        };
        let d = match acc {
            Access::Dead => {
                let msg = if self.b.local(lp.local).kind == LocalKind::Temp {
                    "temporary value dropped while borrowed".to_string()
                } else {
                    format!("`{name}` does not live long enough")
                };
                let d = Diagnostic::error(msg, loan.span)
                    .with_note(format!("`{name}` dropped here while still borrowed"), span);
                later_note(d, "borrow")
            }
            Access::Read | Access::SharedBorrow => {
                let msg = if acc == Access::Read {
                    format!(
                        "cannot use `{}` because it was mutably borrowed",
                        self.b.describe(p)
                    )
                } else {
                    format!(
                        "cannot borrow `{}` as immutable because it is also borrowed as mutable",
                        self.b.describe(p)
                    )
                };
                let d = Diagnostic::error(msg, span)
                    .with_note(format!("mutable borrow of `{name}` occurs here"), loan.span);
                later_note(d, "mutable borrow")
            }
            Access::MutBorrow => {
                let (msg, first) = if loan.mutable {
                    (
                        format!(
                            "cannot borrow `{}` as mutable more than once at a time",
                            self.b.describe(p)
                        ),
                        "first mutable borrow",
                    )
                } else {
                    (format!("cannot borrow `{}` as mutable because it is also borrowed as immutable", self.b.describe(p)), "immutable borrow")
                };
                let d = Diagnostic::error(msg, span)
                    .with_note(format!("{first} occurs here"), loan.span);
                later_note(
                    d,
                    if loan.mutable {
                        "first borrow"
                    } else {
                        "immutable borrow"
                    },
                )
            }
            Access::Write => {
                let d = Diagnostic::error(
                    format!(
                        "cannot assign to `{}` because it is borrowed",
                        self.b.describe(p)
                    ),
                    span,
                )
                .with_note(format!("`{name}` is borrowed here"), loan.span);
                later_note(d, "borrow")
            }
            Access::Move => {
                let d = Diagnostic::error(
                    format!(
                        "cannot move out of `{}` because it is borrowed",
                        self.b.describe(p)
                    ),
                    span,
                )
                .with_note(format!("`{name}` is borrowed here"), loan.span);
                later_note(d, "borrow")
            }
        };
        self.report(d);
    }

    // ----- return values ------------------------------------------------------------------

    fn check_return(&mut self) {
        if self.b.kind != BodyKind::Fn {
            return;
        }
        let mut held: BTreeSet<usize> = BTreeSet::new();
        for bi in 0..self.b.blocks.len() {
            if self.reachable[bi] && matches!(self.b.blocks[bi].term, Terminator::Return) {
                let st = self.holds_before(bi, self.b.blocks[bi].stmts.len());
                held.extend(st[0].iter().copied());
            }
        }
        for li in held {
            let loan = self.loans[li].clone();
            match (&loan.place, loan.param) {
                (Some(lp), _) if !lp.through_ref() => {
                    let decl = self.b.local(lp.local);
                    let name = decl.name.clone().unwrap_or_default();
                    let msg = match decl.kind {
                        LocalKind::User => {
                            format!("cannot return reference to local variable `{name}`")
                        }
                        LocalKind::Arg => {
                            format!("cannot return reference to function parameter `{name}`")
                        }
                        _ => "cannot return reference to a temporary value".to_string(),
                    };
                    self.report(
                        Diagnostic::error(msg, loan.span)
                            .with_plain_note("returns a value referencing data owned by the current function, which is dropped when it returns")
                            .with_help("return an owned value instead (e.g. `String` rather than `&str`)"),
                    );
                }
                (None, Some(idx)) => {
                    let allowed = self.b.return_flows.clone().unwrap_or_default();
                    if allowed.contains(&idx) {
                        continue;
                    }
                    let pname = self.b.locals[idx + 1]
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("parameter {}", idx + 1));
                    let tied = if allowed.is_empty() {
                        "the signature does not tie the result to any parameter".to_string()
                    } else {
                        let names: Vec<String> = allowed
                            .iter()
                            .map(|i| {
                                format!(
                                    "`{}`",
                                    self.b.locals[i + 1].name.clone().unwrap_or_default()
                                )
                            })
                            .collect();
                        format!(
                            "the signature ties the result to {} only",
                            names.join(" and ")
                        )
                    };
                    self.report(
                        Diagnostic::error(format!("lifetime may not live long enough: the returned value borrows from `{pname}`"), self.b.locals[idx + 1].span)
                            .with_plain_note(tied)
                            .with_help(format!("give `{pname}` and the return type the same lifetime, e.g. `<'a>` with `{pname}: &'a T` and `-> &'a U`")),
                    );
                }
                _ => {}
            }
        }
    }

    // ----- tasks ------------------------------------------------------------------------------

    fn implements(&mut self, t: &Ty, tr: &str) -> bool {
        let bounds = self.bounds.clone();
        self.typed.implements(t, tr, &bounds)
    }

    fn check_spawns(&mut self) {
        for bi in 0..self.b.blocks.len() {
            if !self.reachable[bi] {
                continue;
            }
            for si in 0..self.b.blocks[bi].stmts.len() {
                let s = self.b.blocks[bi].stmts[si].clone();
                let StmtKind::Spawn {
                    kind,
                    operands,
                    captures,
                    ..
                } = &s.kind
                else {
                    continue;
                };
                match kind {
                    SpawnKind::GoCall => {
                        for o in operands {
                            let held = Self::op_holds_in(&self.holds_before(bi, si), o);
                            self.check_static(held, s.span, "argument to `go`");
                            if let Some(p) = o.place() {
                                if p.proj.is_empty() {
                                    let t = self.ty_of(p.local).clone();
                                    self.check_send(&t, s.span, None);
                                }
                            }
                        }
                    }
                    SpawnKind::GoBlock { is_move } => {
                        for (c, o) in captures.iter().zip(operands) {
                            if c.mode != CaptureMode::Move {
                                self.report(
                                    Diagnostic::error(format!("`go` block may outlive the current function, but it borrows `{}`", c.name), s.span)
                                        .with_note(format!("`{}` is used here", c.name), c.span)
                                        .with_help("use `go move { ... }` to move captured values into the task (clone them first if you still need them)"),
                                );
                                continue;
                            }
                            let _ = is_move;
                            let held = Self::op_holds_in(&self.holds_before(bi, si), o);
                            self.check_static(held, s.span, "`go` block capture");
                            self.check_send(&c.ty, s.span, Some(&c.name));
                        }
                    }
                    SpawnKind::Scoped => {
                        for c in captures {
                            match c.mode {
                                CaptureMode::Ref => {
                                    if !self.implements(&c.ty, "Sync") {
                                        let t = self.typed.fmt_ty(&c.ty);
                                        self.report(
                                            Diagnostic::error(format!("`{t}` cannot be shared between tasks safely"), s.span)
                                                .with_note(format!("`{}` is borrowed by the task here", c.name), c.span)
                                                .with_help(format!("the task borrows `{}`, so its type must implement `Sync`", c.name)),
                                        );
                                    }
                                }
                                CaptureMode::MutRef | CaptureMode::Move => {
                                    self.check_send(&c.ty, s.span, Some(&c.name))
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Values moved into an unscoped task must not borrow from this function.
    fn check_static(&mut self, held: BTreeSet<usize>, span: Span, what: &str) {
        if let Some(&li) = held.iter().next() {
            let loan = self.loans[li].clone();
            let src = match (&loan.place, loan.param) {
                (Some(p), _) => format!("`{}`", self.b.describe(p)),
                (None, Some(i)) => format!(
                    "parameter `{}`",
                    self.b.locals[i + 1].name.clone().unwrap_or_default()
                ),
                _ => "a local".into(),
            };
            let mut d = Diagnostic::error(format!("{what} borrows {src}, which may not live as long as the task"), span)
                .with_help("tasks started with `go` may outlive the current function: pass an owned value (e.g. `.clone()`), or use `scope(|s| { s.spawn(...) })` for tasks that borrow");
            if loan.place.is_some() {
                d = d.with_note("borrowed here", loan.span);
            }
            self.report(d);
        }
    }

    fn check_send(&mut self, t: &Ty, span: Span, name: Option<&str>) {
        if !self.implements(t, "Send") {
            let tn = self.typed.fmt_ty(t);
            let mut d = Diagnostic::error(format!("`{tn}` cannot be sent between tasks safely"), span)
                .with_help("only `Send` types may move into another task; a type is `Send` when all of its fields are (raw pointers are not)");
            if let Some(n) = name {
                d = d.with_plain_note(format!("the task captures `{n}` of type `{tn}`"));
            }
            self.report(d);
        }
    }
}
