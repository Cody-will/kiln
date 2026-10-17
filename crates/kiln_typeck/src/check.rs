//! Function body checking: inference, coercions, explicit returns, patterns,
//! methods, traits, `?`, channels, and mutability.

use crate::collect::{Checker, Shape};
use crate::ty::{Ty, VarKind};
use kiln_lexer::Span;
use kiln_parser::ast::*;
use kiln_resolve::{Builtin, DefId, DefKind, LocalId, PrimTy, Res};
use std::collections::{HashMap, HashSet};

/// Loop context for `break` typing.
struct LoopCx {
    label: Option<String>,
    /// Type of `break value` for `loop`; `None` for `while`/`for`.
    break_ty: Option<Ty>,
    has_break: bool,
}

/// A resolved method candidate.
struct Method {
    self_param: Option<SelfParam>,
    params: Vec<Ty>,
    ret: Ty,
    /// Trait bound obligations from the method's generics.
    obligations: Vec<(Ty, DefId)>,
    /// Bounds with trait arguments (`I: Iterator<T>`), used to infer `T`
    /// from `I`'s impl.
    arg_obligations: Vec<(Ty, DefId, Vec<Ty>)>,
    /// Index into `Resolved::fns` of the callee, when it is a declared function.
    fn_idx: Option<usize>,
    /// Generic substitution used to instantiate the callee (including `Self` for trait methods).
    subst: HashMap<String, Ty>,
}

/// How a method call was resolved (exported for MIR lowering).
#[derive(Debug, Clone)]
pub struct MethodInfo {
    /// Number of autoderef steps applied to the receiver.
    pub steps: usize,
    /// The callee's `self` parameter (`None` for associated functions).
    pub self_param: Option<SelfParam>,
    /// Receiver type after autoderef.
    pub recv_ty: Ty,
    /// Index into `Resolved::fns` of the callee, if it is a declared method.
    pub fn_idx: Option<usize>,
    /// Generic substitution for the callee (fully resolved).
    pub substs: HashMap<String, Ty>,
}

/// Type information for one function body, exported for later phases.
#[derive(Debug, Clone, Default)]
pub struct FnResults {
    /// Types of locals (fully resolved).
    pub locals: HashMap<LocalId, Ty>,
    /// Types of expressions, keyed by span (fully resolved).
    pub tys: HashMap<Span, Ty>,
    /// Method call resolutions, keyed by the call expression's span.
    pub methods: HashMap<Span, MethodInfo>,
    /// Callee function index for path expressions naming a function.
    pub callees: HashMap<Span, usize>,
    /// Generic substitutions for those callees.
    pub callee_substs: HashMap<Span, HashMap<String, Ty>>,
    /// Paths naming a constant whose value is computed by calling its callee.
    pub const_uses: std::collections::HashSet<Span>,
    /// Callee expressions that are references to function values, with the
    /// number of derefs to reach the function (`f(x)` with `f: &mut fn mut(..)`).
    pub fn_derefs: HashMap<Span, usize>,
    /// Implicit coercions: expression span → the type it was coerced to.
    pub coercions: HashMap<Span, Ty>,
    /// Coercions through user `Deref` impls (`&MutexGuard<Vec<T>>` → `&Vec<T>`):
    /// expression span → number of `deref` calls before any built-in coercion.
    pub deref_coercions: HashMap<Span, usize>,
    /// Trait bounds in scope (`(param, trait)`).
    pub bounds: Vec<(String, DefId)>,
    /// `for` loops over an `Iterator`: loop span → element type. The `next`
    /// method is in `callees`/`callee_substs` under the same span.
    pub iter_items: HashMap<Span, Ty>,
}

/// Per-function checking state.
pub struct Body<'c, 'r, 'a> {
    c: &'c mut Checker<'r, 'a>,
    locals: HashMap<LocalId, Ty>,
    /// Return types of the enclosing function and closures (innermost last).
    rets: Vec<Ty>,
    loops: Vec<LoopCx>,
    bounds: Vec<(String, DefId)>,
    /// Bounds with trait arguments (see `FnSig::bound_args`).
    bound_args: Vec<(String, DefId, Vec<Ty>)>,
    /// Nesting depth of `unsafe` blocks (1 inside an `unsafe fn`).
    unsafe_depth: usize,
    self_ty: Option<Ty>,
    tys: HashMap<Span, Ty>,
    obligations: Vec<(Ty, DefId, Span)>,
    /// Pending `(type, trait, trait args)` bounds whose args are inferred
    /// from the type's impl once it is known.
    arg_obligations: Vec<(Ty, DefId, Vec<Ty>)>,
    declared: Vec<(LocalId, Span)>,
    /// Integer literals, checked against their final type in `finish`.
    int_lits: Vec<(u128, Ty, Span)>,
    /// Integer literals written directly after a unary `-`.
    negated_lits: HashSet<Span>,
    methods: HashMap<Span, MethodInfo>,
    callees: HashMap<Span, usize>,
    callee_substs: HashMap<Span, HashMap<String, Ty>>,
    const_uses: std::collections::HashSet<Span>,
    fn_derefs: HashMap<Span, usize>,
    /// Format specs to check once argument types are known.
    fmt_specs: Vec<(Ty, kiln_parser::ast::FmtSpec, Span)>,
    /// Closures being checked (innermost last) and whether each mutates a
    /// variable declared outside it, which makes an un-annotated closure `fn mut`.
    closure_stack: Vec<(Span, bool)>,
    iter_items: HashMap<Span, Ty>,
    coercions: Vec<(Span, Ty, Ty)>,
    deref_coercions: HashMap<Span, usize>,
    /// Deref steps taken by the last successful `coerces`.
    deref_steps: usize,
    /// Spans of interpolated string literals (which may coerce to `&str`).
    interp_spans: HashSet<Span>,
    /// ADTs whose auto-trait impls are being computed (cycle guard).
    auto_stack: Vec<DefId>,
    /// The expected type of the closure being checked, from the call it is passed to.
    expected_fn: Option<Ty>,
    /// The type expected of the expression being checked, when it sits
    /// directly in a typed position (a return value, an annotated `let`, an
    /// argument). Variant constructors use it so their arguments coerce:
    /// `Some(&msg)` where an `Option<&dyn Error>` is expected.
    expected_ty: Option<Ty>,
    /// Immutable locals declared without an initializer (`let x: T`); their
    /// first assignment is allowed and checked by the borrow checker.
    deferred_init: std::collections::HashSet<LocalId>,
}

impl<'c, 'r, 'a> Body<'c, 'r, 'a> {
    /// A body context with no locals, used for trait queries outside bodies.
    pub(crate) fn detached(c: &'c mut Checker<'r, 'a>, bounds: Vec<(String, DefId)>) -> Self {
        Body {
            c,
            locals: HashMap::new(),
            rets: vec![],
            loops: vec![],
            bounds,
            bound_args: vec![],
            unsafe_depth: 0,
            self_ty: None,
            tys: HashMap::new(),
            obligations: vec![],
            arg_obligations: vec![],
            declared: vec![],
            int_lits: Vec::new(),
            negated_lits: HashSet::new(),
            methods: HashMap::new(),
            callees: HashMap::new(),
            callee_substs: HashMap::new(),
            const_uses: Default::default(),
            fn_derefs: HashMap::new(),
            fmt_specs: Vec::new(),
            closure_stack: Vec::new(),
            iter_items: HashMap::new(),
            coercions: vec![],
            deref_coercions: HashMap::new(),
            deref_steps: 0,
            interp_spans: HashSet::new(),
            auto_stack: vec![],
            expected_fn: None,
            expected_ty: None,
            deferred_init: Default::default(),
        }
    }
}

fn int_ty(p: PrimTy) -> Ty {
    Ty::Prim(p)
}

impl Checker<'_, '_> {
    pub(crate) fn check_bodies(&mut self) {
        for i in 0..self.r.fns.len() {
            let f = &self.r.fns[i];
            if f.decl.body.is_none() {
                continue;
            }
            self.check_fn(i);
        }
        self.check_main();
        self.check_statics();
    }

    /// A `static` is shared by every task, so its type must be `Sync`
    /// (`static mut` is exempt: every use is `unsafe`).
    fn check_statics(&mut self) {
        let Some(sync) = self.r.prelude_type("Sync") else {
            return;
        };
        for &(d, i) in &self.r.statics.clone() {
            let def = self.r.def(d);
            if matches!(
                def.item.map(|it| &it.kind),
                Some(ItemKind::Static { mutable: true, .. })
            ) {
                continue;
            }
            let (name, span) = (def.name.clone(), self.r.fns[i].decl.name.span);
            let t = self.sigs[i].ret.clone();
            if !Body::detached(self, vec![]).implements(&t, sync) {
                let tn = self.fmt_ty(&t);
                self.error_help(
                    format!("static `{name}` has type `{tn}`, which cannot be shared between tasks safely"),
                    span,
                    "statics are shared by every task, so their type must be `Sync`; wrap the value in a `Mutex` or use an atomic",
                );
            }
        }
    }

    fn check_main(&mut self) {
        let Some(&main) = self.r.modules[&self.r.root].values.get("main") else {
            return;
        };
        if self.r.def(main).kind != DefKind::Fn {
            return;
        }
        let Some(i) = self.r.fns.iter().position(|f| f.def == Some(main)) else {
            return;
        };
        let sig = self.sigs[i].clone();
        let span = self.r.fns[i].decl.name.span;
        if !sig.params.is_empty() || !sig.generics.is_empty() {
            self.error("`main` must not take parameters or generic arguments", span);
        }
        let ok = match &sig.ret {
            Ty::Unit => true,
            Ty::Adt(d, args) if self.is_lang(*d, "result") => args[0] == Ty::Unit,
            _ => false,
        };
        if !ok {
            let found = self.fmt_ty(&sig.ret);
            self.error_help(
                format!("`main` has invalid return type `{found}`"),
                span,
                "`main` must return `void` or `result<void, E>`",
            );
        }
    }

    fn check_fn(&mut self, i: usize) {
        let f = self.r.fns[i].clone();
        let sig = self.sigs[i].clone();
        let self_ty = match (f.impl_idx, f.trait_def) {
            (Some(idx), _) => Some(self.impls[idx].self_ty.clone()),
            (None, Some(_)) => Some(Ty::Param("Self".into())),
            _ => None,
        };
        let mut b = Body {
            c: self,
            locals: HashMap::new(),
            rets: vec![sig.ret.clone()],
            loops: Vec::new(),
            bounds: sig.bounds.clone(),
            bound_args: sig.bound_args.clone(),
            unsafe_depth: f.decl.is_unsafe as usize,
            self_ty: self_ty.clone(),
            tys: HashMap::new(),
            obligations: Vec::new(),
            arg_obligations: Vec::new(),
            declared: Vec::new(),
            int_lits: Vec::new(),
            negated_lits: HashSet::new(),
            methods: HashMap::new(),
            callees: HashMap::new(),
            callee_substs: HashMap::new(),
            const_uses: Default::default(),
            fn_derefs: HashMap::new(),
            fmt_specs: Vec::new(),
            closure_stack: Vec::new(),
            iter_items: HashMap::new(),
            coercions: vec![],
            deref_coercions: HashMap::new(),
            deref_steps: 0,
            interp_spans: HashSet::new(),
            auto_stack: Vec::new(),
            expected_fn: None,
            expected_ty: None,
            deferred_init: Default::default(),
        };
        if let (Some(sl), Some(sp), Some(st)) = (f.self_local, &sig.self_param, &self_ty) {
            let t = match sp {
                SelfParam::Value { .. } => st.clone(),
                SelfParam::Ref { mutable, .. } => Ty::Ref(*mutable, Box::new(st.clone())),
            };
            b.locals.insert(sl, t);
        }
        for (p, t) in f.decl.params.iter().zip(&sig.params) {
            b.bind_pat(&p.pat, t, None);
        }
        let body = f.decl.body.as_ref().unwrap();
        let t = b.block(body, false);
        b.check_explicit_return(body, &t, &sig.ret, &f.decl.name);
        b.finish();
        let results = b.export();
        self.results[i] = Some(results);
    }
}

impl Body<'_, '_, '_> {
    fn fresh(&mut self) -> Ty {
        self.c.infer.fresh(VarKind::General)
    }

    fn shallow(&self, t: &Ty) -> Ty {
        self.c.infer.shallow(t)
    }

    fn fmt(&self, t: &Ty) -> String {
        self.c.fmt_ty(t)
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.c.error(msg, span);
    }

    fn error_help(&mut self, msg: impl Into<String>, span: Span, help: impl Into<String>) {
        self.c.error_help(msg, span, help);
    }

    fn lower(&mut self, t: &Type) -> Ty {
        let st = self.self_ty.clone();
        self.c.lower_ty(t, st.as_ref(), true)
    }

    fn export(&mut self) -> FnResults {
        let z = |c: &Checker, t: &Ty| c.infer.zonk(t);
        FnResults {
            locals: self
                .locals
                .iter()
                .map(|(k, v)| (*k, z(self.c, v)))
                .collect(),
            tys: self.tys.iter().map(|(k, v)| (*k, z(self.c, v))).collect(),
            methods: self
                .methods
                .iter()
                .map(|(k, m)| {
                    let mut m = m.clone();
                    m.recv_ty = z(self.c, &m.recv_ty);
                    m.substs = m
                        .substs
                        .iter()
                        .map(|(n, t)| (n.clone(), z(self.c, t)))
                        .collect();
                    (*k, m)
                })
                .collect(),
            callees: self.callees.clone(),
            const_uses: self.const_uses.clone(),
            fn_derefs: self.fn_derefs.clone(),
            callee_substs: self
                .callee_substs
                .iter()
                .map(|(k, m)| {
                    (
                        *k,
                        m.iter().map(|(n, t)| (n.clone(), z(self.c, t))).collect(),
                    )
                })
                .collect(),
            deref_coercions: self.deref_coercions.clone(),
            coercions: self
                .coercions
                .iter()
                .filter_map(|(sp, f, e)| {
                    let (f, e) = (z(self.c, f), z(self.c, e));
                    (f != e && f != Ty::Never).then_some((*sp, e))
                })
                .collect(),
            bounds: self.bounds.clone(),
            iter_items: self
                .iter_items
                .iter()
                .map(|(k, v)| (*k, z(self.c, v)))
                .collect(),
        }
    }

    /// Infers trait arguments from impls: with `I: Iterator<T>` and `I` now
    /// `Counter` (`impl Iterator<i32> for Counter`), `T` is `i32`.
    fn solve_arg_obligations(&mut self) {
        let pending = std::mem::take(&mut self.arg_obligations);
        for (t, tr, args) in pending {
            let t = self.c.infer.zonk(&t);
            match self.shallow(&t) {
                Ty::Var(_) | Ty::Unknown => {
                    self.arg_obligations.push((t, tr, args));
                    continue;
                }
                Ty::Param(n) => {
                    let found = self
                        .bound_args
                        .iter()
                        .find(|(bn, btr, _)| *bn == n && *btr == tr)
                        .map(|(_, _, a)| a.clone());
                    if let Some(have) = found {
                        for (a, b) in args.iter().zip(&have) {
                            self.c.infer.try_unify(a, b);
                        }
                    }
                    continue;
                }
                _ => {}
            }
            // The impls that fit both the type and the arguments known so
            // far. With one, its arguments are adopted; with several
            // (`String: Borrow<?Q>` has `Borrow<String>` and `Borrow<str>`),
            // wait until the arguments are known from elsewhere.
            let mut fits = vec![];
            for idx in 0..self.c.impls.len() {
                let Some((itr, iargs)) = self.c.impls[idx].trait_ref.clone() else {
                    continue;
                };
                if itr != tr {
                    continue;
                }
                let snap = self.c.infer.snapshot();
                let (st, map) = self.instantiate_impl(idx);
                let ok = self.c.infer.try_unify(&st, &t)
                    && args
                        .iter()
                        .zip(&iargs)
                        .all(|(a, b)| self.c.infer.try_unify(a, &b.subst(&map)));
                self.c.infer.restore(snap);
                if ok {
                    fits.push(idx);
                }
            }
            match fits.as_slice() {
                [idx] => {
                    let idx = *idx;
                    let iargs = self.c.impls[idx].trait_ref.clone().unwrap().1;
                    let (st, map) = self.instantiate_impl(idx);
                    self.c.infer.unify(&st, &t);
                    for (a, b) in args.iter().zip(&iargs) {
                        self.c.infer.try_unify(a, &b.subst(&map));
                    }
                }
                [] => {}
                _ => self.arg_obligations.push((t, tr, args)),
            }
        }
    }

    /// Checks that each format spec suits its argument's type.
    fn check_fmt_specs(&mut self) {
        use kiln_parser::ast::FmtKind;
        for (t, spec, span) in std::mem::take(&mut self.fmt_specs) {
            let mut t = self.c.infer.zonk(&t);
            while let Ty::Ref(_, inner) = t {
                t = *inner;
            }
            let int = matches!(&t, Ty::Prim(p) if p.is_integer());
            let float = matches!(t, Ty::Prim(PrimTy::F32 | PrimTy::F64));
            let shown = self.fmt(&t);
            match spec.kind {
                FmtKind::Debug => {
                    if let Some(d) = self.c.r.prelude_type("Debug") {
                        if !self.implements(&t, d) {
                            self.error_help(
                                format!("`{shown}` cannot be formatted with `:?` because it does not implement `Debug`"),
                                span,
                                "add `@derive(Debug)` to the type, or implement `fmt::Debug`",
                            );
                        }
                    }
                }
                FmtKind::LowerHex | FmtKind::UpperHex | FmtKind::Octal | FmtKind::Binary => {
                    if !int {
                        self.error(format!("hexadecimal, octal, and binary formats need an integer, found `{shown}`"), span);
                    }
                }
                FmtKind::LowerExp | FmtKind::UpperExp => {
                    if !int && !float {
                        self.error(
                            format!("scientific notation needs a number, found `{shown}`"),
                            span,
                        );
                    }
                }
                FmtKind::Display => {}
            }
        }
    }

    fn finish(&mut self) {
        self.solve_arg_obligations();
        let obligations = std::mem::take(&mut self.obligations);
        for (t, tr, span) in obligations {
            let t = self.c.infer.zonk(&t);
            if matches!(self.shallow(&t), Ty::Var(_)) {
                continue;
            }
            if !self.implements(&t, tr) {
                let (tn, trn) = (self.fmt(&t), self.c.r.def(tr).name.clone());
                let help = if trn == "Send" || trn == "Sync" {
                    format!("`{trn}` is automatic when every field is `{trn}` (raw pointers are not); if `{tn}` is safe anyway, add `unsafe impl {trn} for {tn} {{}}`")
                } else {
                    format!("add `impl {trn} for {tn}` or `@derive({trn})`")
                };
                self.error_help(
                    format!("the trait `{trn}` is not implemented for `{tn}`"),
                    span,
                    help,
                );
            }
        }
        self.c.infer.apply_literal_defaults();
        self.check_int_literals();
        self.check_fmt_specs();
        let declared = std::mem::take(&mut self.declared);
        for (id, span) in declared {
            let t = self.locals.get(&id).cloned().unwrap_or(Ty::Unknown);
            if self.c.infer.has_unbound(&t) {
                let name = self.c.r.local(id).name.clone();
                let shown = self.fmt(&t);
                self.error_help(
                    format!("type annotations needed for `{name}`"),
                    span,
                    format!("the type is only partially known (`{shown}`); write `let {name}: Type = ...`"),
                );
            }
        }
    }

    /// Rejects integer literals that do not fit their (inferred) type,
    /// instead of letting them wrap.
    fn check_int_literals(&mut self) {
        let lits = std::mem::take(&mut self.int_lits);
        for (v, t, span) in lits {
            let Ty::Prim(p) = self.c.infer.zonk(&t) else {
                continue;
            };
            let (max, signed): (u128, bool) = match p {
                PrimTy::I8 => (i8::MAX as u128, true),
                PrimTy::I16 => (i16::MAX as u128, true),
                PrimTy::I32 => (i32::MAX as u128, true),
                PrimTy::I64 | PrimTy::Isize => (i64::MAX as u128, true),
                PrimTy::I128 => (i128::MAX as u128, true),
                PrimTy::U8 => (u8::MAX as u128, false),
                PrimTy::U16 => (u16::MAX as u128, false),
                PrimTy::U32 => (u32::MAX as u128, false),
                PrimTy::U64 | PrimTy::Usize => (u64::MAX as u128, false),
                PrimTy::U128 => (u128::MAX, false),
                _ => continue,
            };
            let neg = self.negated_lits.contains(&span);
            let limit = if signed && neg { max + 1 } else { max };
            if v > limit {
                let name = self.fmt(&Ty::Prim(p));
                let lo = if signed {
                    format!("-{}", max + 1)
                } else {
                    "0".to_string()
                };
                self.error_help(
                    format!("literal out of range for `{name}`"),
                    span,
                    format!("`{name}` holds {lo} to {max}; give the literal a wider type with a suffix (e.g. `{v}i64`, `{v}u64`)"),
                );
            }
        }
    }

    /// Enforces SPEC §3.1: non-unit functions must end every path with `return`.
    fn check_explicit_return(&mut self, body: &Block, body_ty: &Ty, ret: &Ty, name: &Ident) {
        let ret = self.c.infer.zonk(ret);
        if matches!(ret, Ty::Unit) || matches!(self.shallow(body_ty), Ty::Never) {
            return;
        }
        if let Some(Stmt {
            kind: StmtKind::Expr(e),
            ..
        }) = body.stmts.last()
        {
            let t = self.tys.get(&e.span).cloned().unwrap_or(Ty::Unit);
            if !matches!(self.shallow(&t), Ty::Unit | Ty::Never) && !e.is_block_like() {
                self.error_help(
                    "missing `return`: Kiln functions do not return their last expression implicitly",
                    e.span,
                    "write `return <expr>`",
                );
                return;
            }
        }
        let r = self.fmt(&ret);
        let end = Span::new(body.span.end.saturating_sub(1), body.span.end);
        self.error_help(
            format!(
                "function `{}` must return a value of type `{r}` on every path",
                name.name
            ),
            end,
            "add a `return` statement before the end of the function",
        );
    }

    // ----- coercion ------------------------------------------------------------

    /// Checks that `found` can be used where `expected` is required, reporting a
    /// mismatch at `span`. Supports `!`, `&mut T → &T`, and deref coercions
    /// (`&String → &str`, `&Vec<T> → &[T]`, `&[T; N] → &[T]`, `&Box<T> → &T`, `&T → &dyn Trait`).
    fn coerce(&mut self, found: &Ty, expected: &Ty, span: Span) -> bool {
        if self.coerces(found, expected) {
            if self.deref_steps > 0 {
                self.deref_coercions.insert(span, self.deref_steps);
            }
            self.coercions.push((span, found.clone(), expected.clone()));
            return true;
        }
        // An interpolated string literal can be used where a `&str` is
        // expected: `open("{dir}/{name}")`. The `String` lives until the end
        // of the enclosing block.
        if self.interp_spans.contains(&span) {
            if let (Ty::Adt(sd, _), Ty::Ref(false, inner)) =
                (self.shallow(found), self.shallow(expected))
            {
                if self.c.is_lang(sd, "string")
                    && matches!(self.shallow(&inner), Ty::Prim(PrimTy::Str))
                {
                    self.coercions.push((span, found.clone(), expected.clone()));
                    return true;
                }
            }
        }
        let (e, f) = (self.fmt(expected), self.fmt(found));
        let mut d = kiln_lexer::Diagnostic::error(
            format!("mismatched types: expected `{e}`, found `{f}`"),
            span,
        );
        if let (Ty::Adt(sd, _), Ty::Ref(_, inner)) = (self.shallow(found), self.shallow(expected)) {
            if self.c.is_lang(sd, "string") && matches!(self.shallow(&inner), Ty::Prim(PrimTy::Str))
            {
                d = d.with_help("borrow it: `&value` (a `&String` coerces to `&str`)");
            }
        }
        self.c.errors.push(d);
        false
    }

    fn coerces(&mut self, found: &Ty, expected: &Ty) -> bool {
        self.deref_steps = 0;
        let (f, e) = (self.shallow(found), self.shallow(expected));
        if matches!(f, Ty::Never) {
            return true;
        }
        // A shareable `fn` can be used where a `fn mut` is expected.
        if let (Ty::Fn(fp, fr, false), Ty::Fn(_, _, true)) = (&f, &e) {
            let as_mut = Ty::Fn(fp.clone(), fr.clone(), true);
            return self.c.infer.try_unify(&as_mut, &e);
        }
        if let (Ty::Ref(fm, fi), Ty::Ref(em, ei)) = (&f, &e) {
            if *em && !*fm {
                return false;
            }
            let (fi, ei) = (self.shallow(fi), self.shallow(ei));
            if self.c.infer.try_unify(&fi, &ei) {
                return true;
            }
            match (&fi, &ei) {
                (Ty::Adt(d, _), Ty::Prim(PrimTy::Str)) if self.c.is_lang(*d, "string") => {
                    return true
                }
                (Ty::Adt(d, args), Ty::Slice(t)) if self.c.is_lang(*d, "Vec") => {
                    return self.c.infer.unify(&args[0], t)
                }
                (Ty::Array(a, _), Ty::Slice(t)) => return self.c.infer.unify(a, t),
                (Ty::Adt(d, args), _) if self.c.is_lang(*d, "Box") => {
                    return self.c.infer.try_unify(&args[0], &ei)
                }
                (_, Ty::Dyn(traits)) => {
                    let traits = traits.clone();
                    return traits.iter().all(|t| self.implements(&fi, *t));
                }
                // `&&T` → `&T` (auto-deref through references).
                (Ty::Ref(m2, _), _) => {
                    if *em && !*m2 {
                        return false;
                    }
                    let next = fi.clone();
                    if self.coerces(&next, &e) {
                        self.deref_steps += 1;
                        return true;
                    }
                    return false;
                }
                // Deref coercion through a user `Deref` (`DerefMut` for
                // `&mut`): `&MutexGuard<Vec<T>>` → `&Vec<T>` → `&[T]`.
                (Ty::Adt(..), _) => {
                    let tn = if *em { "DerefMut" } else { "Deref" };
                    let Some(u) = self.overloaded_deref(&fi, tn) else {
                        return false;
                    };
                    let next = Ty::Ref(*em, Box::new(u));
                    if self.coerces(&next, &e) {
                        self.deref_steps += 1;
                        return true;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        if let (Ty::Adt(d, fa), Ty::Adt(d2, ea)) = (&f, &e) {
            if d == d2 && self.c.is_lang(*d, "Box") {
                let (fi, ei) = (self.shallow(&fa[0]), self.shallow(&ea[0]));
                if let (Ty::Dyn(traits), false) = (&ei, matches!(fi, Ty::Dyn(_) | Ty::Var(_))) {
                    let traits = traits.clone();
                    return traits.iter().all(|t| self.implements(&fi, *t));
                }
            }
        }
        if let (Ty::Adt(d, args), Ty::Dyn(traits)) = (&f, &e) {
            if self.c.is_lang(*d, "Box") {
                let inner = args[0].clone();
                let traits = traits.clone();
                return traits.iter().all(|t| self.implements(&inner, *t));
            }
        }
        self.c.infer.try_unify(&f, &e)
    }

    // ----- traits ----------------------------------------------------------------

    fn supertrait_closure(&self, tr: DefId) -> Vec<DefId> {
        let mut out = vec![tr];
        let mut i = 0;
        while i < out.len() {
            if let Some(info) = self.c.traits.get(&out[i]) {
                for s in &info.supertraits {
                    if !out.contains(s) {
                        out.push(*s);
                    }
                }
            }
            i += 1;
        }
        out
    }

    fn param_bounds(&self, name: &str) -> Vec<DefId> {
        let mut out = Vec::new();
        for (n, t) in &self.bounds {
            if n == name {
                for s in self.supertrait_closure(*t) {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
        out
    }

    /// Whether `t` implements trait `tr`.
    pub fn implements(&mut self, t: &Ty, tr: DefId) -> bool {
        let t = self.c.infer.zonk(t);
        let trait_name = self.c.r.def(tr).name.clone();
        let tn = trait_name.as_str();
        let marker = |names: &[&str]| names.contains(&tn);
        match &t {
            Ty::Var(v) => match self.c.infer.kind(*v) {
                VarKind::Int => self.implements(&Ty::Prim(PrimTy::I32), tr),
                VarKind::Float => self.implements(&Ty::Prim(PrimTy::F64), tr),
                VarKind::General => true,
            },
            Ty::Unknown | Ty::Never => true,
            Ty::Param(n) => {
                let n = n.clone();
                self.param_bounds(&n).contains(&tr) || self.has_impl(&t, tr)
            }
            Ty::Prim(p) => {
                (match p {
                    PrimTy::F32 | PrimTy::F64 => marker(&[
                        "Copy", "Clone", "Debug", "Display", "Default", "Send", "Sync",
                    ]),
                    PrimTy::Str => {
                        marker(&["Eq", "Ord", "Hash", "Debug", "Display", "Send", "Sync"])
                    }
                    _ => marker(&[
                        "Copy", "Clone", "Eq", "Ord", "Hash", "Debug", "Display", "Default",
                        "Send", "Sync",
                    ]),
                }) || self.has_impl(&t, tr)
            }
            Ty::Unit => marker(&[
                "Copy", "Clone", "Eq", "Ord", "Hash", "Debug", "Default", "Send", "Sync",
            ]),
            Ty::Ref(m, inner) => {
                if marker(&["Copy", "Clone"]) {
                    !*m
                } else if tn == "Send" && !*m {
                    // `&T: Send` iff `T: Sync`.
                    let inner = (**inner).clone();
                    match self.c.r.prelude_type("Sync") {
                        Some(sync) => self.implements(&inner, sync),
                        None => true,
                    }
                } else {
                    let inner = (**inner).clone();
                    self.implements(&inner, tr)
                }
            }
            Ty::Tuple(ts) => {
                let ts = ts.clone();
                ts.iter().all(|x| self.implements(x, tr))
            }
            Ty::Array(x, _) | Ty::Slice(x) => {
                let x = (**x).clone();
                self.implements(&x, tr)
            }
            Ty::ChanSend(x) | Ty::ChanRecv(x) if marker(&["Send", "Sync"]) => {
                let x = (**x).clone();
                match self.c.r.prelude_type("Send") {
                    Some(send) => self.implements(&x, send),
                    None => true,
                }
            }
            Ty::Fn(_, _, true) => false,
            Ty::Fn(..) | Ty::ChanSend(_) | Ty::ChanRecv(_) => {
                marker(&["Copy", "Clone", "Send", "Sync"])
                    && !(marker(&["Copy"]) && matches!(t, Ty::ChanSend(_) | Ty::ChanRecv(_)))
            }
            Ty::Ptr(..) => marker(&["Copy", "Clone"]),
            Ty::Dyn(ts) => ts.iter().any(|d| self.supertrait_closure(*d).contains(&tr)),
            Ty::Adt(d, args) => {
                let d = *d;
                if self.c.r.is_core(d)
                    && !self.c.adts[&d].is_enum
                    && self.c.r.def(d).name == "string"
                {
                    return marker(&[
                        "Clone", "Eq", "Ord", "Hash", "Debug", "Display", "Default", "Send", "Sync",
                    ]) || self.has_impl(&t, tr);
                }
                if self.c.r.is_core(d) {
                    let args = args.clone();
                    let structural = marker(&[
                        "Clone", "Eq", "Ord", "Hash", "Debug", "Default", "Send", "Sync",
                    ]) || (tn == "Copy" && self.c.r.def(d).name == "option");
                    return (structural && args.iter().all(|a| self.implements(a, tr)))
                        || self.has_impl(&t, tr);
                }
                if marker(&["Send", "Sync"]) {
                    // Auto traits: an explicit impl wins; otherwise all fields must implement it.
                    if self.has_impl(&t, tr) || self.auto_stack.contains(&d) {
                        return true;
                    }
                    let info = self.c.adts[&d].clone();
                    let map: HashMap<String, Ty> = info
                        .generics
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect();
                    let mut fields: Vec<Ty> = Vec::new();
                    let mut push_shape = |sh: &Shape| match sh {
                        Shape::Unit => {}
                        Shape::Tuple(ts) => fields.extend(ts.iter().map(|x| x.subst(&map))),
                        Shape::Named(fs) => fields.extend(fs.iter().map(|(_, x)| x.subst(&map))),
                    };
                    push_shape(&info.shape);
                    for (_, sh) in &info.variants {
                        push_shape(sh);
                    }
                    self.auto_stack.push(d);
                    let ok = fields.iter().all(|f| self.implements(f, tr));
                    self.auto_stack.pop();
                    return ok;
                }
                self.c.adts[&d].derives.iter().any(|x| x == tn) || self.has_impl(&t, tr)
            }
        }
    }

    pub(crate) fn has_impl_pub(&mut self, t: &Ty, tr: DefId) -> bool {
        self.has_impl(t, tr)
    }

    /// Whether an explicit `impl Trait for T` matches `t`.
    fn has_impl(&mut self, t: &Ty, tr: DefId) -> bool {
        for idx in 0..self.c.impls.len() {
            if self.c.impls[idx].trait_ref.as_ref().map(|x| x.0) != Some(tr) {
                continue;
            }
            let (st, map) = self.instantiate_impl(idx);
            if !self.c.infer.can_unify(&st, t) {
                continue;
            }
            // `impl<T: Bound> Trait for X<T>` applies only when the bounds hold.
            let bounds = self.c.impls[idx].bounds.clone();
            if bounds.is_empty() {
                return true;
            }
            let snap = self.c.infer.snapshot();
            self.c.infer.unify(&st, t);
            let ok = bounds.iter().all(|(g, b)| match map.get(g) {
                Some(v) => {
                    let v = self.c.infer.zonk(v);
                    matches!(v, Ty::Var(_)) || self.implements(&v, *b)
                }
                None => true,
            });
            self.c.infer.restore(snap);
            if ok {
                return true;
            }
        }
        false
    }

    /// Instantiates an impl's generics with fresh variables; returns its self type and the map.
    fn instantiate_impl(&mut self, idx: usize) -> (Ty, HashMap<String, Ty>) {
        let generics = self.c.impls[idx].generics.clone();
        let map: HashMap<String, Ty> = generics
            .into_iter()
            .map(|g| (g, self.c.infer.fresh(VarKind::General)))
            .collect();
        (self.c.impls[idx].self_ty.subst(&map), map)
    }

    // ----- method lookup ---------------------------------------------------------

    /// Instantiates signature `i` with `subst` for outer generics and fresh vars
    /// for the method's own generics.
    fn instantiate_sig(&mut self, i: usize, mut subst: HashMap<String, Ty>) -> Method {
        let sig = self.c.sigs[i].clone();
        for g in sig.generics.iter().chain(&sig.outer_generics) {
            if !subst.contains_key(g) {
                let v = self.fresh();
                subst.insert(g.clone(), v);
            }
        }
        let obligations = sig
            .bounds
            .iter()
            .filter(|(n, _)| n != "Self")
            .filter_map(|(n, t)| subst.get(n).map(|ty| (ty.clone(), *t)))
            .collect();
        let arg_obligations = sig
            .bound_args
            .iter()
            .filter(|(n, _, args)| n != "Self" && !args.is_empty())
            .filter_map(|(n, t, args)| {
                let args = args.iter().map(|a| a.subst(&subst)).collect();
                subst.get(n).map(|ty| (ty.clone(), *t, args))
            })
            .collect();
        Method {
            arg_obligations,
            self_param: sig.self_param.clone(),
            params: sig.params.iter().map(|t| t.subst(&subst)).collect(),
            ret: sig.ret.subst(&subst),
            obligations,
            fn_idx: Some(i),
            subst,
        }
    }

    /// Finds method `name` on exactly type `t` (no autoderef).
    fn method_on(&mut self, t: &Ty, name: &str) -> Option<Method> {
        let t = self.shallow(t);
        let trait_method = |s: &mut Self, traits: Vec<DefId>, self_t: &Ty| -> Option<Method> {
            for tr in traits {
                for sup in s.supertrait_closure(tr) {
                    if let Some(&i) = s.c.traits.get(&sup).and_then(|ti| ti.methods.get(name)) {
                        let mut subst = HashMap::new();
                        subst.insert("Self".to_string(), self_t.clone());
                        return Some(s.instantiate_sig(i, subst));
                    }
                }
            }
            None
        };
        match &t {
            Ty::Param(n) => {
                // A bound with trait arguments (`I: Iterator<T>`) instantiates
                // the trait's parameters with them.
                for (bn, tr, args) in self.bound_args.clone() {
                    if &bn != n {
                        continue;
                    }
                    let tinfo = self.c.traits.get(&tr).cloned().unwrap_or_default();
                    if let Some(&i) = tinfo.methods.get(name) {
                        let mut subst: HashMap<String, Ty> =
                            tinfo.generics.iter().cloned().zip(args).collect();
                        subst.insert("Self".to_string(), t.clone());
                        return Some(self.instantiate_sig(i, subst));
                    }
                }
                let traits = self.param_bounds(n);
                return trait_method(self, traits, &t);
            }
            Ty::Dyn(traits) => return trait_method(self, traits.clone(), &t),
            // An integer or float literal of not-yet-known type has the
            // `Eq`/`Ord`/`Hash` methods every numeric type has.
            Ty::Var(v) if !matches!(self.c.infer.kind(*v), VarKind::General) => {
                return self.builtin_trait_method(&t, name);
            }
            Ty::Var(_) | Ty::Unknown => return None,
            _ => {}
        }
        // Inherent impls first, then trait impls (including default methods).
        for pass in 0..2 {
            for idx in 0..self.c.impls.len() {
                let is_trait = self.c.impls[idx].trait_ref.is_some();
                if is_trait != (pass == 1) {
                    continue;
                }
                // `impl Trait for &mut T` serves trait bounds; method calls on a
                // reference use the referent's impl (one autoderef step later).
                if matches!(t, Ty::Ref(..)) && matches!(self.c.impls[idx].self_ty, Ty::Ref(..)) {
                    continue;
                }
                let (st, map) = self.instantiate_impl(idx);
                if !self.c.infer.can_unify(&st, &t) {
                    continue;
                }
                if let Some(&i) = self.c.impls[idx].methods.get(name) {
                    self.c.infer.unify(&st, &t);
                    return Some(self.instantiate_sig(i, map));
                }
                if let Some((tr, args)) = self.c.impls[idx].trait_ref.clone() {
                    let tinfo = self.c.traits.get(&tr).cloned().unwrap_or_default();
                    if let Some(&i) = tinfo.methods.get(name) {
                        self.c.infer.unify(&st, &t);
                        let mut subst: HashMap<String, Ty> = tinfo
                            .generics
                            .iter()
                            .cloned()
                            .zip(args.iter().map(|a| a.subst(&map)))
                            .collect();
                        subst.insert("Self".into(), t.clone());
                        return Some(self.instantiate_sig(i, subst));
                    }
                }
            }
        }
        if matches!(t, Ty::Ref(..)) {
            // Autoderef finds them on the referent.
            return None;
        }
        self.builtin_trait_method(&t, name)
    }

    /// Built-in and derived implementations of `Eq`, `Ord`, and `Hash` have
    /// no impl block; their methods are the traits' defaults.
    fn builtin_trait_method(&mut self, t: &Ty, name: &str) -> Option<Method> {
        let t = t.clone();
        for tn in ["Eq", "Ord", "Hash", "Debug"] {
            let Some(tr) = self.c.r.prelude_type(tn) else {
                continue;
            };
            let Some(&i) = self.c.traits.get(&tr).and_then(|ti| ti.methods.get(name)) else {
                continue;
            };
            if self.implements(&t, tr) {
                let mut subst = HashMap::new();
                subst.insert("Self".to_string(), t.clone());
                return Some(self.instantiate_sig(i, subst));
            }
        }
        None
    }

    /// The target of a user `Deref` (`trait_name` = "Deref") or `DerefMut`
    /// impl for `t`, if any.
    pub(crate) fn overloaded_deref(&mut self, t: &Ty, trait_name: &str) -> Option<Ty> {
        let t = self.shallow(t);
        let Ty::Adt(d, _) = &t else { return None };
        if ["Box", "string", "Vec"]
            .iter()
            .any(|n| self.c.is_lang(*d, n))
        {
            return None;
        }
        let tr = self.c.r.prelude_type(trait_name)?;
        for idx in 0..self.c.impls.len() {
            let Some((itr, args)) = self.c.impls[idx].trait_ref.clone() else {
                continue;
            };
            if itr != tr {
                continue;
            }
            let (st, map) = self.instantiate_impl(idx);
            if self.c.infer.can_unify(&st, &t) {
                self.c.infer.unify(&st, &t);
                return args.first().map(|a| a.subst(&map));
            }
        }
        None
    }

    /// One autoderef step.
    fn deref_step(&mut self, t: &Ty) -> Option<Ty> {
        if let Some(u) = self.overloaded_deref(t, "Deref") {
            return Some(u);
        }
        match self.shallow(t) {
            Ty::Ref(_, x) | Ty::Ptr(_, x) => Some(*x),
            Ty::Adt(d, args) if self.c.is_lang(d, "Box") => Some(args[0].clone()),
            Ty::Adt(d, _) if self.c.is_lang(d, "string") => Some(Ty::Prim(PrimTy::Str)),
            Ty::Adt(d, args) if self.c.is_lang(d, "Vec") => {
                Some(Ty::Slice(Box::new(args[0].clone())))
            }
            // Arrays have the slice methods (`arr.iter()`, `arr.len()`).
            Ty::Array(x, _) => Some(Ty::Slice(x)),
            _ => None,
        }
    }

    /// Strips references only.
    fn strip_refs(&self, t: &Ty) -> Ty {
        let mut t = self.shallow(t);
        while let Ty::Ref(_, x) = t {
            t = self.shallow(&x);
        }
        t
    }

    // ----- statements and blocks ---------------------------------------------------

    /// Checks a block. When `value` is true, the block's last expression
    /// statement gives its value (used for `if`/`match` arms in value position).
    fn block(&mut self, b: &Block, value: bool) -> Ty {
        let mut diverges = false;
        let n = b.stmts.len();
        let mut last = Ty::Unit;
        for (i, s) in b.stmts.iter().enumerate() {
            let is_last = i + 1 == n;
            let t = self.stmt(s, value && is_last);
            if matches!(self.shallow(&t), Ty::Never) {
                diverges = true;
            }
            if is_last {
                last = t;
            }
        }
        let tail_is_expr = matches!(
            b.stmts.last(),
            Some(Stmt {
                kind: StmtKind::Expr(_),
                ..
            })
        );
        if value && tail_is_expr {
            last
        } else if diverges {
            Ty::Never
        } else {
            Ty::Unit
        }
    }

    fn stmt(&mut self, s: &Stmt, value: bool) -> Ty {
        match &s.kind {
            StmtKind::Let { pat, ty, init } => {
                if init.is_none() {
                    if let PatternKind::Ident { name, .. } = &pat.kind {
                        if let Some(&id) = self.c.r.bindings.get(&name.span) {
                            self.deferred_init.insert(id);
                        }
                    }
                }
                let declared = ty.as_ref().map(|t| self.lower(t));
                let t = match (&declared, init) {
                    (Some(d), Some(e)) => {
                        let f = self.expr_expect(e, d);
                        self.coerce(&f, d, e.span);
                        d.clone()
                    }
                    (Some(d), None) => d.clone(),
                    (None, Some(e)) => self.expr(e),
                    (None, None) => self.fresh(),
                };
                let never = matches!(self.shallow(&t), Ty::Never);
                self.bind_pat(pat, &t, None);
                if let PatternKind::Ident { name, .. } = &pat.kind {
                    if let Some(&id) = self.c.r.bindings.get(&name.span) {
                        self.declared.push((id, name.span));
                    }
                }
                if never {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
            StmtKind::ShortDecl { names, tuple, init } => {
                let t = self.expr(init);
                if matches!(self.shallow(&t), Ty::Never) {
                    return Ty::Never;
                }
                let elems: Vec<Ty> = if *tuple {
                    let vars: Vec<Ty> = names.iter().map(|_| self.fresh()).collect();
                    let tup = Ty::Tuple(vars.clone());
                    if !self.c.infer.unify(&t, &tup) {
                        let f = self.fmt(&t);
                        self.error(
                            format!("cannot destructure `{f}` into {} names; expected a tuple with {} elements", names.len(), names.len()),
                            init.span,
                        );
                        names.iter().map(|_| Ty::Unknown).collect()
                    } else {
                        vars
                    }
                } else {
                    if matches!(self.shallow(&t), Ty::Unit) {
                        self.error(
                            "cannot declare a variable from an expression of type `void`",
                            init.span,
                        );
                    }
                    vec![t]
                };
                for (n, t) in names.iter().zip(elems) {
                    if let Some(id) = n.name.as_ref().and_then(|i| self.c.r.bindings.get(&i.span)) {
                        self.locals.insert(*id, t);
                        self.declared.push((*id, n.name.as_ref().unwrap().span));
                    }
                }
                Ty::Unit
            }
            StmtKind::Assign { target, op, value } => {
                let vt = self.expr(value);
                let tt = self.expr(target);
                self.check_place_mut(target, "assign to");
                match op {
                    None => {
                        self.coerce(&vt, &tt, value.span);
                    }
                    Some(op) => {
                        self.arith(*op, &tt, &vt, s.span);
                    }
                }
                if matches!(self.shallow(&vt), Ty::Never) {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
            StmtKind::Send { chan, value } => {
                let ct = self.expr(chan);
                let vt = self.expr(value);
                let elem = self.fresh();
                let want = Ty::ChanSend(Box::new(elem.clone()));
                let ct_stripped = self.strip_refs(&ct);
                if !self.c.infer.unify(&ct_stripped, &want) {
                    let f = self.fmt(&ct);
                    self.error_help(
                        format!("cannot send on a value of type `{f}`"),
                        chan.span,
                        "sending requires a send-only channel `chan<- T`",
                    );
                } else {
                    self.coerce(&vt, &elem, value.span);
                }
                Ty::Unit
            }
            StmtKind::Defer(e) => {
                self.expr(e);
                Ty::Unit
            }
            StmtKind::Item(_) => Ty::Unit,
            StmtKind::Expr(e) => self.expr_v(e, value),
        }
    }

    /// Checks that `e` is a mutable place; `what` is e.g. "assign to".
    /// Mutating through a user `Deref` type `t` (the value of `base`): needs a
    /// `DerefMut` impl and a mutable `base`.
    fn check_deref_mut(&mut self, t: &Ty, base: &Expr, what: &str) {
        if self.overloaded_deref(t, "DerefMut").is_none() {
            let tn = self.fmt(t);
            self.error_help(
                format!("cannot {what} data through `{tn}`"),
                base.span,
                format!(
                    "`{tn}` implements `Deref` but not `DerefMut`, so it only gives shared access"
                ),
            );
            return;
        }
        match self.shallow(t) {
            Ty::Ref(true, _) => {}
            _ => self.check_place_mut(base, "borrow"),
        }
    }

    fn check_place_mut(&mut self, e: &Expr, what: &str) {
        match &e.kind {
            ExprKind::Path(p) => match self.c.r.path(p.span) {
                Res::Local(id) => {
                    let l = self.c.r.local(id).clone();
                    for (cs, mutates) in self.closure_stack.iter_mut() {
                        if l.span.start < cs.start || l.span.start >= cs.end {
                            *mutates = true;
                        }
                    }
                    let deferred = what == "assign to" && self.deferred_init.contains(&id);
                    if !(l.mutable || deferred) {
                        let msg = if what == "assign to" {
                            format!("cannot assign twice to immutable variable `{}`", l.name)
                        } else {
                            format!(
                                "cannot {what} `{}`, as it is not declared as mutable",
                                l.name
                            )
                        };
                        self.error_help(
                            msg,
                            e.span,
                            format!(
                                "make it mutable: `{} mut := ...` or `let mut {} = ...`",
                                l.name, l.name
                            ),
                        );
                    }
                }
                Res::Def(d)
                    if matches!(
                        self.c.r.def(d).item.map(|i| &i.kind),
                        Some(ItemKind::Static { mutable: true, .. })
                    ) => {}
                Res::Def(d) if self.c.r.def(d).kind == DefKind::Static => {
                    let n = self.c.r.def(d).name.clone();
                    self.error_help(
                        format!("cannot {what} the immutable static `{n}`"),
                        e.span,
                        "shared statics change through interior mutability (`AtomicI64`, `Mutex`); `static mut` needs `unsafe`",
                    );
                }
                Res::Err | Res::External => {}
                _ => self.error(format!("cannot {what} this expression"), e.span),
            },
            ExprKind::Field(base, _) | ExprKind::TupleIndex(base, _) | ExprKind::Index(base, _) => {
                let bt = self.tys.get(&base.span).cloned().unwrap_or(Ty::Unknown);
                // Writing a field reached through a user `Deref` needs `DerefMut`.
                if let (ExprKind::Field(_, name), Ty::Adt(d, _)) = (&e.kind, self.shallow(&bt)) {
                    let direct = match &self.c.adts[&d].shape {
                        Shape::Named(fs) => fs.iter().any(|(n, _)| *n == name.name),
                        _ => false,
                    };
                    if !direct && self.overloaded_deref(&bt, "Deref").is_some() {
                        self.check_deref_mut(&bt, base, what);
                        return;
                    }
                }
                if let (ExprKind::Index(..), Ty::Adt(..)) = (&e.kind, self.shallow(&bt)) {
                    if self.overloaded_deref(&bt, "Deref").is_some() {
                        self.check_deref_mut(&bt, base, what);
                        return;
                    }
                }
                match self.shallow(&bt) {
                    Ty::Ref(true, _) | Ty::Ptr(true, _) => {}
                    Ty::Ref(false, _) => self.error_help(
                        format!("cannot {what} data behind a `&` reference"),
                        e.span,
                        "take a `&mut` reference instead",
                    ),
                    _ => self.check_place_mut(base, what),
                }
            }
            ExprKind::Unary(UnOp::Deref, inner) => {
                let it = self.tys.get(&inner.span).cloned().unwrap_or(Ty::Unknown);
                if let Ty::Adt(..) = self.shallow(&it) {
                    self.check_deref_mut(&it, inner, what);
                    return;
                }
                if let Ty::Ref(false, _) = self.shallow(&it) {
                    self.error_help(
                        format!("cannot {what} data behind a `&` reference"),
                        e.span,
                        "take a `&mut` reference instead",
                    );
                }
            }
            ExprKind::Paren(inner) => self.check_place_mut(inner, what),
            // A temporary (the result of a call, a literal, ...) can be
            // borrowed mutably; it lives until the end of the statement.
            ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StructLit { .. }
            | ExprKind::Tuple(_)
            | ExprKind::Array(_)
            | ExprKind::Lit(_)
            | ExprKind::Range { .. }
                if what == "borrow" => {}
            _ => self.error(format!("cannot {what} this expression"), e.span),
        }
    }

    // ----- expressions -----------------------------------------------------------

    /// Checks an expression in value position.
    fn expr(&mut self, e: &Expr) -> Ty {
        self.expr_v(e, true)
    }

    /// Checks `e` where a value of type `expected` is wanted.
    fn expr_expect(&mut self, e: &Expr, expected: &Ty) -> Ty {
        self.expected_ty = Some(expected.clone());
        let t = self.expr(e);
        self.expected_ty = None;
        t
    }

    fn expr_v(&mut self, e: &Expr, value: bool) -> Ty {
        // An expectation applies to this expression only, never to the
        // expressions nested inside it.
        if !matches!(e.kind, ExprKind::Call(..)) {
            self.expected_ty = None;
        }
        let t = self.expr_inner(e, value);
        self.tys.insert(e.span, t.clone());
        t
    }

    fn lit_ty(&mut self, l: &Lit) -> Ty {
        match l {
            Lit::Int {
                suffix: Some(s), ..
            }
            | Lit::Float {
                suffix: Some(s), ..
            } => PrimTy::from_name(s).map(Ty::Prim).unwrap_or(Ty::Unknown),
            Lit::Int { .. } => self.c.infer.fresh(VarKind::Int),
            Lit::Float { .. } => self.c.infer.fresh(VarKind::Float),
            Lit::Str(_) => Ty::r#ref(Ty::Prim(PrimTy::Str)),
            Lit::ByteStr(_) => Ty::r#ref(Ty::Slice(Box::new(Ty::Prim(PrimTy::U8)))),
            Lit::Char(_) => Ty::Prim(PrimTy::Char),
            Lit::Bool(_) => Ty::Prim(PrimTy::Bool),
        }
    }

    fn expr_inner(&mut self, e: &Expr, value: bool) -> Ty {
        match &e.kind {
            ExprKind::Lit(l) => {
                let t = self.lit_ty(l);
                if let Lit::Int { value, .. } = l {
                    self.int_lits.push((*value, t.clone(), e.span));
                }
                t
            }
            ExprKind::Interp(parts) => {
                self.interp_spans.insert(e.span);
                for p in parts {
                    if let InterpPart::Expr(x, spec) = p {
                        let t = self.expr(x);
                        if let Some(spec) = spec {
                            self.fmt_specs.push((t, spec.clone(), x.span));
                        }
                    }
                }
                self.c.string_ty()
            }
            ExprKind::Path(p) => self.path_expr(p, e.span),
            ExprKind::Paren(x) => self.expr_v(x, value),
            ExprKind::Unary(op, x) => self.unary(*op, x, e.span),
            ExprKind::Binary(op, l, r) => {
                let lt = self.expr(l);
                let rt = self.expr(r);
                self.binary(*op, &lt, &rt, e.span)
            }
            ExprKind::Cast(x, t) => {
                let from = self.expr(x);
                let to = self.lower(t);
                self.check_cast(&from, &to, e.span);
                to
            }
            ExprKind::Call(f, args) => {
                let expected = self.expected_ty.take();
                self.call_expect(f, args, e.span, expected)
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => self.method_call(receiver, method, args, e.span),
            ExprKind::Field(base, name) => {
                let bt = self.expr(base);
                self.field(&bt, name)
            }
            ExprKind::TupleIndex(base, i) => {
                let bt = self.expr(base);
                self.tuple_index(&bt, *i, e.span)
            }
            ExprKind::Index(base, idx) => {
                let bt = self.expr(base);
                let it = self.expr(idx);
                self.index(&bt, &it, e.span)
            }
            ExprKind::Try(x) => {
                let t = self.expr(x);
                self.try_op(&t, e.span)
            }
            ExprKind::Range { start, end, .. } => {
                let v = self.fresh();
                for x in [start, end].into_iter().flatten() {
                    let t = self.expr(x);
                    if !self.c.infer.unify(&t, &v) {
                        let (a, b) = (self.fmt(&v), self.fmt(&t));
                        self.error(format!("mismatched range bounds: `{a}` and `{b}`"), x.span);
                    }
                }
                Ty::Adt(self.c.lang("Range"), vec![v])
            }
            ExprKind::StructLit { path, fields, base } => {
                self.struct_lit(path, fields, base.as_deref(), e.span)
            }
            ExprKind::Tuple(xs) if xs.is_empty() => Ty::Unit,
            ExprKind::Tuple(xs) => Ty::Tuple(xs.iter().map(|x| self.expr(x)).collect()),
            ExprKind::Array(xs) => {
                let elem = self.fresh();
                for x in xs {
                    let t = self.expr(x);
                    self.coerce(&t, &elem, x.span);
                }
                Ty::Array(Box::new(elem), xs.len() as u64)
            }
            ExprKind::ArrayRepeat(x, n) => {
                let t = self.expr(x);
                let nt = self.expr(n);
                self.coerce(&nt, &Ty::Prim(PrimTy::Usize), n.span);
                let len = crate::collect::array_len(n).unwrap_or_else(|| {
                    self.error("array lengths must be integer literals for now", n.span);
                    0
                });
                {
                    // `[x; N]` copies `x`: it must be `Copy` when N > 1.
                    let zt = self.c.infer.zonk(&t);
                    if len > 1
                        && !self.c.infer.has_unbound(&zt)
                        && self
                            .c
                            .r
                            .prelude_type("Copy")
                            .is_some_and(|c| !self.implements(&zt, c))
                    {
                        self.error(
                            format!(
                                "`[x; {len}]` needs a `Copy` element type, found `{}`",
                                self.fmt(&zt)
                            ),
                            x.span,
                        );
                    }
                }
                Ty::Array(Box::new(t), len)
            }
            ExprKind::Block(b) => self.block(b, value),
            ExprKind::Unsafe(b) => {
                self.unsafe_depth += 1;
                let t = self.block(b, value);
                self.unsafe_depth -= 1;
                t
            }
            ExprKind::If { cond, then, els } => {
                let ct = self.expr(cond);
                self.coerce(&ct, &Ty::Prim(PrimTy::Bool), cond.span);
                self.if_tail(then, els.as_deref(), value, e.span)
            }
            ExprKind::IfLet {
                pat,
                expr,
                then,
                els,
            } => {
                let t = self.expr(expr);
                self.bind_pat(pat, &t, None);
                self.if_tail(then, els.as_deref(), value, e.span)
            }
            ExprKind::Match(scrut, arms) => self.match_expr(scrut, arms, value, e.span),
            ExprKind::Select(arms) => {
                for arm in arms {
                    match &arm.op {
                        SelectOp::Recv { bind, chan } => {
                            let ct = self.expr(chan);
                            let elem = self.recv_elem(&ct, chan.span);
                            if let Some(id) = bind
                                .as_ref()
                                .and_then(|b| b.name.as_ref())
                                .and_then(|n| self.c.r.bindings.get(&n.span))
                            {
                                self.locals.insert(*id, elem);
                            }
                        }
                        SelectOp::Send { chan, value } => {
                            let ct = self.expr(chan);
                            let vt = self.expr(value);
                            let elem = self.fresh();
                            let ct = self.strip_refs(&ct);
                            if self
                                .c
                                .infer
                                .unify(&ct, &Ty::ChanSend(Box::new(elem.clone())))
                            {
                                self.coerce(&vt, &elem, value.span);
                            } else {
                                let f = self.fmt(&ct);
                                self.error(
                                    format!("cannot send on a value of type `{f}`"),
                                    chan.span,
                                );
                            }
                        }
                        SelectOp::Default => {}
                    }
                    self.expr_v(&arm.body, false);
                }
                Ty::Unit
            }
            ExprKind::For {
                label,
                pat,
                iter,
                body,
            } => {
                let it = self.expr(iter);
                let elem = self.iter_elem(&it, iter.span, e.span);
                self.bind_pat(pat, &elem, None);
                self.loop_body(label, None, body);
                Ty::Unit
            }
            ExprKind::While { label, cond, body } => {
                let ct = self.expr(cond);
                self.coerce(&ct, &Ty::Prim(PrimTy::Bool), cond.span);
                self.loop_body(label, None, body);
                Ty::Unit
            }
            ExprKind::WhileLet {
                label,
                pat,
                expr,
                body,
            } => {
                let t = self.expr(expr);
                self.bind_pat(pat, &t, None);
                self.loop_body(label, None, body);
                Ty::Unit
            }
            ExprKind::Loop { label, body } => {
                let bt = self.fresh();
                let cx = self.loop_body(label, Some(bt.clone()), body);
                if cx.has_break {
                    let t = self.shallow(&bt);
                    if let Ty::Var(v) = t {
                        if self.c.infer.kind(v) == VarKind::General {
                            self.c.infer.unify(&t, &Ty::Unit);
                        }
                    }
                    bt
                } else {
                    Ty::Never
                }
            }
            ExprKind::Break(label, val) => {
                let vt = val.as_ref().map(|v| (self.expr(v), v.span));
                let target = match label {
                    Some(l) => self
                        .loops
                        .iter()
                        .rposition(|c| c.label.as_deref() == Some(l.name.as_str())),
                    None => self.loops.len().checked_sub(1),
                };
                if let Some(i) = target {
                    self.loops[i].has_break = true;
                    match (&self.loops[i].break_ty.clone(), vt) {
                        (Some(bt), Some((t, sp))) => {
                            self.coerce(&t, bt, sp);
                        }
                        (Some(bt), None) => {
                            if !self.c.infer.unify(bt, &Ty::Unit) {
                                let b = self.fmt(bt);
                                self.error(
                                    format!(
                                        "`break` without a value in a `loop` that produces `{b}`"
                                    ),
                                    e.span,
                                );
                            }
                        }
                        (None, Some((_, sp))) => {
                            self.error("`break` with a value is only allowed inside `loop`", sp)
                        }
                        (None, None) => {}
                    }
                }
                Ty::Never
            }
            ExprKind::Continue(_) => Ty::Never,
            ExprKind::Return(v) => {
                let ret = self.rets.last().cloned().unwrap_or(Ty::Unit);
                match v {
                    Some(x) => {
                        let t = self.expr_expect(x, &ret);
                        if matches!(self.c.infer.zonk(&ret), Ty::Unit)
                            && !matches!(self.shallow(&t), Ty::Unit | Ty::Never)
                        {
                            let f = self.fmt(&t);
                            self.error_help(
                                format!("mismatched types: this function returns `void` but a value of type `{f}` was returned"),
                                x.span,
                                format!("add the return type `{f}` after the parameters"),
                            );
                        } else {
                            self.coerce(&t, &ret, x.span);
                        }
                    }
                    None => {
                        if !self.c.infer.unify(&ret, &Ty::Unit) {
                            let r = self.fmt(&ret);
                            self.error(
                                format!(
                                    "`return` without a value in a function that returns `{r}`"
                                ),
                                e.span,
                            );
                        }
                    }
                }
                Ty::Never
            }
            ExprKind::Closure {
                params, ret, body, ..
            } => {
                let (expected, is_mut) = match self.expected_fn.take() {
                    Some(Ty::Fn(ps, _, m)) if ps.len() == params.len() => (Some(ps), m),
                    _ => (None, false),
                };
                let expected_given = expected.is_some();
                let pts: Vec<Ty> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| match (&p.ty, &expected) {
                        (Some(t), _) => self.lower(t),
                        (None, Some(ps)) => ps[i].clone(),
                        (None, None) => self.fresh(),
                    })
                    .collect();
                for (p, t) in params.iter().zip(&pts) {
                    self.bind_pat(&p.pat, t, None);
                }
                let rt = match ret {
                    Some(t) => self.lower(t),
                    None => self.fresh(),
                };
                self.rets.push(rt.clone());
                let saved = std::mem::take(&mut self.loops);
                self.closure_stack.push((e.span, false));
                match &body.kind {
                    ExprKind::Block(b) => {
                        let bt = self.block(b, false);
                        if !matches!(self.shallow(&bt), Ty::Never)
                            && !self.c.infer.try_unify(&rt, &Ty::Unit)
                        {
                            let r = self.fmt(&rt);
                            self.error_help(
                                format!("closure must return a value of type `{r}` on every path"),
                                body.span,
                                "add a `return` statement",
                            );
                        }
                    }
                    _ => {
                        let bt = self.expr(body);
                        self.coerce(&bt, &rt, body.span);
                    }
                }
                self.loops = saved;
                self.rets.pop();
                let (_, mutates) = self.closure_stack.pop().unwrap_or_default();
                // Without an expected type, a closure that mutates what it
                // captures is a `fn mut`.
                let is_mut = if expected_given { is_mut } else { mutates };
                Ty::Fn(pts, Box::new(rt), is_mut)
            }
            ExprKind::Spawn(g) => {
                // `spawn f(x)`: the task's result is the call's; a `spawn` block
                // returns with `return v` (unit when it has none).
                let t = match &**g {
                    SpawnTarget::Call(call) => self.expr(call),
                    SpawnTarget::Block { body, .. } => {
                        let rt = self.fresh();
                        self.rets.push(rt.clone());
                        let saved = std::mem::take(&mut self.loops);
                        let bt = self.block(body, false);
                        self.loops = saved;
                        self.rets.pop();
                        if !matches!(self.shallow(&bt), Ty::Never)
                            && !self.c.infer.try_unify(&rt, &Ty::Unit)
                        {
                            let r = self.fmt(&rt);
                            self.error_help(
                                format!(
                                    "`spawn` block must return a value of type `{r}` on every path"
                                ),
                                body.span,
                                "add a `return` statement",
                            );
                        }
                        rt
                    }
                };
                match self.c.r.prelude_type("Task") {
                    Some(d) => Ty::Adt(d, vec![t]),
                    None => Ty::Unknown,
                }
            }
            ExprKind::Error => Ty::Unknown,
        }
    }

    fn loop_body(&mut self, label: &Option<Ident>, break_ty: Option<Ty>, body: &Block) -> LoopCx {
        self.loops.push(LoopCx {
            label: label.as_ref().map(|l| l.name.clone()),
            break_ty,
            has_break: false,
        });
        self.block(body, false);
        self.loops.pop().unwrap()
    }

    fn if_tail(&mut self, then: &Block, els: Option<&Expr>, value: bool, span: Span) -> Ty {
        let tt = self.block(then, value);
        match els {
            None => {
                if value && !matches!(self.shallow(&tt), Ty::Unit | Ty::Never) {
                    let t = self.fmt(&tt);
                    self.error_help(
                        format!("`if` without `else` used as a value of type `{t}`"),
                        span,
                        "add an `else` branch",
                    );
                }
                Ty::Unit
            }
            Some(e) => {
                let et = self.expr_v(e, value);
                if value {
                    self.join(&tt, &et, e.span)
                } else if matches!(self.shallow(&tt), Ty::Never)
                    && matches!(self.shallow(&et), Ty::Never)
                {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
        }
    }

    /// Joins branch types (`!` defers to the other branch).
    fn join(&mut self, a: &Ty, b: &Ty, span: Span) -> Ty {
        match (self.shallow(a), self.shallow(b)) {
            (Ty::Never, _) => b.clone(),
            (_, Ty::Never) => a.clone(),
            _ => {
                if !self.coerces(b, a) && !self.coerces(a, b) {
                    let (x, y) = (self.fmt(a), self.fmt(b));
                    self.error(
                        format!("branches have incompatible types: expected `{x}`, found `{y}`"),
                        span,
                    );
                }
                a.clone()
            }
        }
    }

    fn recv_elem(&mut self, ct: &Ty, span: Span) -> Ty {
        let elem = self.fresh();
        let ct = self.strip_refs(ct);
        if self
            .c
            .infer
            .unify(&ct, &Ty::ChanRecv(Box::new(elem.clone())))
        {
            elem
        } else {
            let f = self.fmt(&ct);
            self.error_help(
                format!("cannot receive from a value of type `{f}`"),
                span,
                "receiving requires a receive-only channel `<-chan T`",
            );
            Ty::Unknown
        }
    }

    fn iter_elem(&mut self, t: &Ty, span: Span, for_span: Span) -> Ty {
        // Any `Iterator`: the loop calls `next` (recorded under the loop's span).
        if let Some(elem) = self.iterator_next(t, for_span) {
            return elem;
        }
        match self.shallow(t) {
            Ty::Unknown => Ty::Unknown,
            Ty::Adt(d, args) if self.c.is_lang(d, "Range") || self.c.is_lang(d, "Vec") => {
                args[0].clone()
            }
            Ty::Array(x, _) | Ty::Slice(x) => *x,
            Ty::ChanRecv(x) => *x,
            Ty::Ref(m, inner) => match self.shallow(&inner) {
                Ty::Adt(d, args) if self.c.is_lang(d, "Vec") => {
                    Ty::Ref(m, Box::new(args[0].clone()))
                }
                Ty::Array(x, _) | Ty::Slice(x) => Ty::Ref(m, x),
                Ty::Unknown => Ty::Unknown,
                _ => self.not_iterable(t, span),
            },
            _ => self.not_iterable(t, span),
        }
    }

    /// When `t` is an `Iterator` (and not a built-in iterable), records its
    /// `next` method under `for_span` and returns the element type.
    fn iterator_next(&mut self, t: &Ty, for_span: Span) -> Option<Ty> {
        let t = self.shallow(t);
        match &t {
            Ty::Adt(d, _) if self.c.is_lang(*d, "Range") || self.c.is_lang(*d, "Vec") => {
                return None
            }
            Ty::Adt(..) | Ty::Param(_) => {}
            _ => return None,
        }
        let tr = self.c.r.prelude_type("Iterator")?;
        if !self.implements(&t, tr) {
            return None;
        }
        let m = self.method_on(&t, "next")?;
        let i = m.fn_idx?;
        self.callees.insert(for_span, i);
        self.callee_substs.insert(for_span, m.subst.clone());
        let elem = match self.shallow(&m.ret) {
            Ty::Adt(o, args) if Some(o) == self.c.r.prelude_type("option") => args.first().cloned(),
            _ => None,
        }?;
        self.iter_items.insert(for_span, elem.clone());
        Some(elem)
    }

    fn not_iterable(&mut self, t: &Ty, span: Span) -> Ty {
        let f = self.fmt(t);
        self.error(format!("`{f}` is not iterable"), span);
        Ty::Unknown
    }

    // ----- paths -------------------------------------------------------------------

    fn ctor_ty(&mut self, adt: DefId, shape: &Shape, args: &[Ty], what: &str, span: Span) -> Ty {
        let generics = self.c.adts[&adt].generics.clone();
        let map: HashMap<String, Ty> = generics.into_iter().zip(args.iter().cloned()).collect();
        let me = Ty::Adt(adt, args.to_vec());
        match shape {
            Shape::Unit => me,
            Shape::Tuple(ts) => Ty::Fn(
                ts.iter().map(|t| t.subst(&map)).collect(),
                Box::new(me),
                false,
            ),
            Shape::Named(_) => {
                self.error_help(
                    format!("expected value, found struct-like {what}"),
                    span,
                    format!("use a struct literal: `{what} {{ ... }}`"),
                );
                Ty::Unknown
            }
        }
    }

    fn fresh_args(&mut self, adt: DefId, explicit: Vec<Ty>) -> Vec<Ty> {
        let n = self.c.adts[&adt].generics.len();
        if explicit.len() == n {
            explicit
        } else {
            (0..n).map(|_| self.fresh()).collect()
        }
    }

    fn seg_args(&mut self, p: &Path, seg: usize) -> Vec<Ty> {
        let args = p
            .segments
            .get(seg)
            .map(|s| s.args.clone())
            .unwrap_or_default();
        args.iter()
            .filter_map(|a| match a {
                GenericArg::Type(t) => Some(self.lower(t)),
                _ => None,
            })
            .collect()
    }

    fn variant_info(&self, v: DefId) -> (DefId, Shape) {
        let DefKind::Variant { parent, index } = self.c.r.def(v).kind else {
            unreachable!()
        };
        (parent, self.c.adts[&parent].variants[index].1.clone())
    }

    fn path_expr(&mut self, p: &Path, span: Span) -> Ty {
        let res = self.c.r.path(p.span);
        match res {
            Res::Local(id) => self.locals.get(&id).cloned().unwrap_or(Ty::Unknown),
            Res::Def(d) => {
                let def = self.c.r.def(d);
                match def.kind {
                    DefKind::Fn => {
                        let Some(i) = self.c.r.fns.iter().position(|f| f.def == Some(d)) else {
                            return Ty::Unknown;
                        };
                        let m = self.instantiate_sig(i, HashMap::new());
                        let explicit = self.seg_args(p, p.segments.len() - 1);
                        if !explicit.is_empty() {
                            let gens = self.c.sigs[i].generics.clone();
                            // Re-instantiate with explicit turbofish arguments.
                            if gens.len() == explicit.len() {
                                let map = gens.into_iter().zip(explicit).collect();
                                let m = self.instantiate_sig(i, map);
                                return self.method_fn_ty(m, span);
                            }
                            self.error(
                                format!(
                                    "function takes {} generic arguments but {} were supplied",
                                    self.c.sigs[i].generics.len(),
                                    explicit.len()
                                ),
                                span,
                            );
                        }
                        self.method_fn_ty(m, span)
                    }
                    DefKind::Const | DefKind::Static => match def.item.map(|i| &i.kind) {
                        Some(ItemKind::Const { ty, value, .. }) => {
                            let literal =
                                matches!(value, Some(v) if matches!(v.kind, ExprKind::Lit(_)));
                            if let (false, Some(&i)) = (literal, self.c.r.const_fns.get(&d)) {
                                self.callees.insert(span, i);
                                self.callee_substs.insert(span, HashMap::new());
                                self.const_uses.insert(span);
                            }
                            let ty = ty.clone();
                            self.c
                                .lower_ty(&kiln_resolve::static_lifetimes(&ty), None, false)
                        }
                        Some(ItemKind::Static { ty, mutable, .. }) => {
                            if *mutable {
                                self.require_unsafe("using a `static mut`", span);
                            }
                            let ty = ty.clone();
                            self.c.lower_ty(&ty, None, false)
                        }
                        _ => Ty::Unknown,
                    },
                    DefKind::Variant { .. } => {
                        let (adt, shape) = self.variant_info(d);
                        // `None::<i32>` or `Option::<i32>::None`.
                        let last = p.segments.len() - 1;
                        let explicit = if !p.segments[last].args.is_empty() {
                            self.seg_args(p, last)
                        } else if p.segments.len() >= 2 {
                            self.seg_args(p, p.segments.len() - 2)
                        } else {
                            vec![]
                        };
                        let args = self.fresh_args(adt, explicit);
                        let name = self.c.r.def(d).name.clone();
                        self.ctor_ty(adt, &shape, &args, &name, span)
                    }
                    DefKind::Struct => {
                        let shape = self.c.adts[&d].shape.clone();
                        let explicit = self.seg_args(p, p.segments.len() - 1);
                        let args = self.fresh_args(d, explicit);
                        let name = self.c.r.def(d).name.clone();
                        self.ctor_ty(d, &shape, &args, &name, span)
                    }
                    _ => Ty::Unknown,
                }
            }
            Res::Assoc { base, name } => self.assoc(p, &base, &name, span),
            Res::Builtin(Builtin::Drop) => {
                let v = self.fresh();
                Ty::Fn(vec![v], Box::new(Ty::Unit), false)
            }
            Res::Builtin(Builtin::Chan) => {
                self.error("`chan` must be called: `chan::<T>(capacity)`", span);
                Ty::Unknown
            }
            _ => Ty::Unknown,
        }
    }

    /// Function type for a method used as a value or path call (`Type::method`),
    /// with `self` as the first parameter.
    /// Reports `what` unless inside an `unsafe` block or `unsafe fn`.
    fn require_unsafe(&mut self, what: &str, span: Span) {
        if self.unsafe_depth == 0 {
            self.error_help(
                format!("{what} is unsafe and requires an `unsafe` block"),
                span,
                "wrap it in `unsafe { ... }` after checking the safety requirements",
            );
        }
    }

    /// Checks a call to function `i`: an `unsafe fn` needs an `unsafe` block.
    fn check_unsafe_call(&mut self, i: usize, span: Span) {
        if self.c.r.fns[i].decl.is_unsafe {
            let name = self.c.r.fns[i].decl.name.name.clone();
            self.require_unsafe(&format!("calling `unsafe fun {name}`"), span);
        }
    }

    fn method_fn_ty(&mut self, m: Method, span: Span) -> Ty {
        if let Some(i) = m.fn_idx {
            self.check_unsafe_call(i, span);
            self.callees.insert(span, i);
            self.callee_substs.insert(span, m.subst.clone());
        }
        for (t, tr) in m.obligations {
            self.obligations.push((t, tr, span));
        }
        self.arg_obligations.extend(m.arg_obligations);
        Ty::Fn(m.params, Box::new(m.ret), false)
    }

    /// `Base::name` paths: associated functions, methods (UFCS), and `Self::Variant`.
    fn assoc(&mut self, p: &Path, base: &Res, name: &str, span: Span) -> Ty {
        let base_ty = match base {
            Res::Def(d) if matches!(self.c.r.def(*d).kind, DefKind::Struct | DefKind::Enum) => {
                let explicit = self.seg_args(p, p.segments.len() - 2);
                let args = self.fresh_args(*d, explicit);
                Ty::Adt(*d, args)
            }
            Res::Prim(pr) => Ty::Prim(*pr),
            Res::SelfTy => self.self_ty.clone().unwrap_or(Ty::Param("Self".into())),
            Res::TyParam(n) => Ty::Param(n.clone()),
            Res::Def(d) if self.c.r.def(*d).kind == DefKind::Trait => {
                // `Trait::method`: Self is inferred from the first argument.
                let tr = *d;
                if let Some(&i) = self.c.traits.get(&tr).and_then(|t| t.methods.get(name)) {
                    let selft = self.fresh();
                    let mut map = HashMap::new();
                    map.insert("Self".to_string(), selft.clone());
                    let m = self.instantiate_sig(i, map);
                    self.obligations.push((selft.clone(), tr, span));
                    return self.with_self_param(m, selft, span);
                }
                let tn = self.c.r.def(tr).name.clone();
                self.error(format!("trait `{tn}` has no method `{name}`"), span);
                return Ty::Unknown;
            }
            _ => return Ty::Unknown,
        };
        // `Self::Variant` / `Enum::Variant` through Self.
        if let Ty::Adt(d, args) = &base_ty {
            if self.c.adts[d].is_enum {
                let vs = self.c.adts[d].variants.clone();
                if let Some((v, shape)) = vs.iter().find(|(v, _)| self.c.r.def(*v).name == name) {
                    let _ = v;
                    return self.ctor_ty(*d, shape, args, name, span);
                }
            }
        }
        match self.method_on(&base_ty, name) {
            Some(m) if m.fn_idx.is_some_and(|i| self.c.r.const_fn_set.contains(&i)) => {
                // An associated constant: its value is a call.
                let ret = m.ret.clone();
                self.method_fn_ty(m, span);
                self.const_uses.insert(span);
                ret
            }
            Some(m) => self.with_self_param(m, base_ty, span),
            None => {
                let msg = match &base_ty {
                    Ty::Adt(d, _) if self.c.adts[d].is_enum => format!(
                        "no variant or associated function named `{name}` in enum `{}`",
                        self.c.r.def(*d).name
                    ),
                    _ => {
                        let t = self.fmt(&base_ty);
                        format!("no function or associated item named `{name}` found for `{t}`")
                    }
                };
                self.error(msg, span);
                Ty::Unknown
            }
        }
    }

    fn with_self_param(&mut self, mut m: Method, self_ty: Ty, span: Span) -> Ty {
        match &m.self_param {
            Some(SelfParam::Value { .. }) => m.params.insert(0, self_ty),
            Some(SelfParam::Ref { mutable, .. }) => {
                m.params.insert(0, Ty::Ref(*mutable, Box::new(self_ty)))
            }
            None => {}
        }
        self.method_fn_ty(m, span)
    }

    // ----- calls -----------------------------------------------------------------

    fn is_print_fn(&self, f: &Expr) -> bool {
        let ExprKind::Path(p) = &f.kind else {
            return false;
        };
        let Res::Def(d) = self.c.r.path(p.span) else {
            return false;
        };
        self.c.r.is_core(d)
            && matches!(
                self.c.r.def(d).name.as_str(),
                "print" | "println" | "eprint" | "eprintln"
            )
    }

    fn call_expect(&mut self, f: &Expr, args: &[Expr], span: Span, expected: Option<Ty>) -> Ty {
        if let ExprKind::Path(p) = &f.kind {
            if self.c.r.path(p.span) == Res::Builtin(Builtin::Chan) {
                let explicit = self.seg_args(p, p.segments.len() - 1);
                let elem = explicit
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| self.c.infer.fresh(VarKind::General));
                if args.len() > 1 {
                    self.error(
                        format!(
                            "`chan` takes at most 1 argument (the capacity) but {} were supplied",
                            args.len()
                        ),
                        span,
                    );
                }
                for a in args {
                    let t = self.expr(a);
                    self.coerce(&t, &int_ty(PrimTy::Usize), a.span);
                }
                // Values sent over a channel move to another task.
                if let Some(send) = self.c.r.prelude_type("Send") {
                    self.obligations.push((elem.clone(), send, span));
                }
                return Ty::Tuple(vec![
                    Ty::ChanSend(Box::new(elem.clone())),
                    Ty::ChanRecv(Box::new(elem)),
                ]);
            }
        }
        if self.is_print_fn(f) {
            for a in args {
                self.expr(a);
            }
            if args.len() > 1 {
                self.error_help(
                    "print functions take a single argument",
                    span,
                    "use string interpolation: \"{a} {b}\"",
                );
            }
            return Ty::Unit;
        }
        let mut ft = self.expr(f);
        // Calling through references: `f(x)` with `f: &fn(..)` or `&mut fn mut(..)`.
        let mut derefs = 0;
        let mut shared = false;
        while let Ty::Ref(m, inner) = self.shallow(&ft) {
            if !matches!(self.shallow(&inner), Ty::Fn(..) | Ty::Ref(..)) {
                break;
            }
            shared |= !m;
            ft = *inner;
            derefs += 1;
        }
        if derefs > 0 {
            if shared && matches!(self.shallow(&ft), Ty::Fn(_, _, true)) {
                self.error_help(
                    "cannot call a `fun mut` through a shared reference",
                    f.span,
                    "pass it as `&mut fn mut(...)`",
                );
            }
            self.fn_derefs.insert(f.span, derefs);
        }
        match self.shallow(&ft) {
            Ty::Fn(params, ret, _) => {
                // A variant constructor takes its type from the expected type
                // when they agree, so its arguments are coercion sites.
                if let (Some(exp), ExprKind::Path(p)) = (&expected, &f.kind) {
                    if let Res::Def(d) = self.c.r.path(p.span) {
                        if matches!(self.c.r.def(d).kind, DefKind::Variant { .. })
                            && self.c.infer.can_unify(&ret, exp)
                        {
                            self.c.infer.unify(&ret, exp);
                        }
                    }
                }
                self.check_args(&params, args, span);
                self.solve_arg_obligations();
                *ret
            }
            Ty::Unknown => {
                for a in args {
                    self.expr(a);
                }
                Ty::Unknown
            }
            Ty::Var(v) if self.c.infer.kind(v) == VarKind::General => {
                let pts: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();
                let ret = self.fresh();
                self.c
                    .infer
                    .unify(&ft, &Ty::Fn(pts, Box::new(ret.clone()), false));
                ret
            }
            other => {
                let t = self.fmt(&other);
                self.error(format!("expected function, found `{t}`"), f.span);
                for a in args {
                    self.expr(a);
                }
                Ty::Unknown
            }
        }
    }

    fn check_args(&mut self, params: &[Ty], args: &[Expr], span: Span) {
        if params.len() != args.len() {
            self.error(
                format!(
                    "this function takes {} argument{} but {} {} supplied",
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
                span,
            );
        }
        for (i, a) in args.iter().enumerate() {
            if let (ExprKind::Closure { .. }, Some(p)) = (&a.kind, params.get(i)) {
                let p = self.shallow(p);
                if matches!(p, Ty::Fn(..)) {
                    self.expected_fn = Some(p);
                }
            }
            let t = match params.get(i) {
                Some(p) => {
                    let p = p.clone();
                    self.expr_expect(a, &p)
                }
                None => self.expr(a),
            };
            if let Some(p) = params.get(i) {
                self.coerce(&t, p, a.span);
            }
        }
    }

    fn method_call(&mut self, recv: &Expr, method: &Ident, args: &[Expr], span: Span) -> Ty {
        let rt = self.expr(recv);
        let name = method.name.as_str();
        // Autoderef: try the receiver type, then each deref step.
        let mut cands = vec![rt.clone()];
        while let Some(next) = self.deref_step(cands.last().unwrap()) {
            if cands.len() > 8 {
                break;
            }
            cands.push(next);
        }
        for (steps, cand) in cands.iter().enumerate() {
            let c = self.shallow(cand);
            if matches!(c, Ty::Unknown) {
                for a in args {
                    self.expr(a);
                }
                return Ty::Unknown;
            }
            if let Some(t) = self.builtin_method(&c, name, args, span) {
                let self_param = Some(SelfParam::Ref {
                    lifetime: None,
                    mutable: false,
                });
                let info = MethodInfo {
                    steps,
                    self_param,
                    recv_ty: c.clone(),
                    fn_idx: None,
                    substs: HashMap::new(),
                };
                self.methods.insert(span, info);
                return t;
            }
            if let Some(m) = self.method_on(&c, name) {
                let info = MethodInfo {
                    steps,
                    self_param: m.self_param.clone(),
                    recv_ty: c.clone(),
                    fn_idx: m.fn_idx,
                    substs: m.subst.clone(),
                };
                self.methods.insert(span, info);
                if let Some(SelfParam::Ref { mutable: true, .. }) = m.self_param {
                    // The first user-`Deref` step on the way, if any.
                    let overloaded = (0..steps).find(|&k| {
                        let c = cands[k].clone();
                        self.overloaded_deref(&c, "Deref").is_some()
                    });
                    if let Some(k) = overloaded {
                        let c = cands[k].clone();
                        if k == 0 {
                            self.check_deref_mut(&c, recv, "borrow");
                        } else if self.overloaded_deref(&c, "DerefMut").is_none() {
                            let tn = self.fmt(&c);
                            self.error(format!("cannot borrow data through `{tn}` mutably: it does not implement `DerefMut`"), recv.span);
                        }
                    } else if steps == 0 {
                        self.check_place_mut(recv, "borrow");
                    } else if let Ty::Ref(false, _) = self.shallow(&cands[0]) {
                        self.error_help(
                            format!("cannot call `{name}` (which takes `&mut self`) through a `&` reference"),
                            recv.span,
                            "take a `&mut` reference instead",
                        );
                    }
                }
                for (t, tr) in &m.obligations {
                    self.obligations.push((t.clone(), *tr, span));
                }
                self.arg_obligations.extend(m.arg_obligations.clone());
                if let Some(i) = m.fn_idx {
                    self.check_unsafe_call(i, span);
                }
                self.check_args(&m.params, args, span);
                self.solve_arg_obligations();
                return m.ret;
            }
        }
        for a in args {
            self.expr(a);
        }
        let t = self.fmt(&rt);
        if matches!(self.shallow(&rt), Ty::Var(_)) {
            self.error_help(
                format!("type annotations needed before calling `.{name}()`"),
                recv.span,
                "annotate the receiver's type",
            );
        } else {
            self.error(
                format!("no method named `{name}` found for type `{t}`"),
                method.span,
            );
        }
        Ty::Unknown
    }

    /// Methods the compiler provides for all types or for channels.
    fn builtin_method(&mut self, c: &Ty, name: &str, args: &[Expr], span: Span) -> Option<Ty> {
        let ret = match (c, name) {
            (Ty::ChanSend(x), "try_send") => {
                self.check_args(&[(**x).clone()], args, span);
                let err = Ty::Adt(self.c.lang("TrySendError"), vec![(**x).clone()]);
                return Some(self.c.result_ty(Ty::Unit, err));
            }
            (Ty::ChanRecv(x), "try_recv") => {
                let err = Ty::Adt(self.c.lang("TryRecvError"), vec![]);
                self.c.result_ty((**x).clone(), err)
            }
            (Ty::ChanRecv(x), "recv") => self.c.option_ty((**x).clone()),
            (Ty::ChanSend(_), "close") => Ty::Unit,
            (Ty::ChanSend(_) | Ty::ChanRecv(_), "len" | "cap") => Ty::Prim(PrimTy::Usize),
            (Ty::ChanSend(_) | Ty::ChanRecv(_), "clone") => c.clone(),
            (
                Ty::Adt(..) | Ty::Prim(_) | Ty::Tuple(_) | Ty::Unit | Ty::Param(_) | Ty::Array(..),
                "clone",
            ) => {
                // A hand-written `impl Clone` is an ordinary method.
                if let (Ty::Adt(..), Some(cl)) = (c, self.c.r.prelude_type("Clone")) {
                    if self.has_impl(c, cl) {
                        return None;
                    }
                }
                if let Some(cl) = self.c.r.prelude_type("Clone") {
                    self.obligations.push((c.clone(), cl, span));
                }
                c.clone()
            }
            (Ty::Prim(_) | Ty::Adt(..) | Ty::Param(_), "to_string")
                if !matches!(c, Ty::Prim(PrimTy::Str)) =>
            {
                if self.method_on(c, name).is_some() {
                    return None;
                }
                // `boxed.to_string()` on a `Box<dyn Display>` (or another
                // smart pointer) reaches the pointee through autoderef.
                if let (Ty::Adt(..), Some(d)) = (c, self.c.r.prelude_type("Display")) {
                    if !self.implements(c, d) && self.deref_step(c).is_some() {
                        return None;
                    }
                }
                if let Some(d) = self.c.r.prelude_type("Display") {
                    self.obligations.push((c.clone(), d, span));
                }
                self.c.string_ty()
            }
            (Ty::Slice(x) | Ty::Array(x, _), "len") => {
                let _ = x;
                Ty::Prim(PrimTy::Usize)
            }
            (Ty::Slice(_) | Ty::Array(..), "is_empty") => Ty::Prim(PrimTy::Bool),
            _ => return None,
        };
        self.check_args(&[], args, span);
        Some(ret)
    }

    // ----- fields and indexing -------------------------------------------------------

    fn field(&mut self, bt: &Ty, name: &Ident) -> Ty {
        let mut t = bt.clone();
        loop {
            match self.shallow(&t) {
                Ty::Adt(d, args) if !self.c.adts[&d].is_enum => {
                    let info = self.c.adts[&d].clone();
                    if let Shape::Named(fs) = &info.shape {
                        if let Some((_, ft)) = fs.iter().find(|(n, _)| *n == name.name) {
                            let map = info.generics.iter().cloned().zip(args).collect();
                            return ft.subst(&map);
                        }
                    }
                }
                Ty::Unknown => return Ty::Unknown,
                _ => {}
            }
            let is_vec_or_string = matches!(self.shallow(&t), Ty::Adt(d, _) if self.c.is_lang(d, "string") || self.c.is_lang(d, "Vec"));
            match self.deref_step(&t) {
                Some(n) if !is_vec_or_string => t = n,
                _ => break,
            }
        }
        let tn = self.fmt(bt);
        self.error(
            format!("no field `{}` on type `{tn}`", name.name),
            name.span,
        );
        Ty::Unknown
    }

    fn tuple_index(&mut self, bt: &Ty, i: u32, span: Span) -> Ty {
        let t = self.strip_refs(bt);
        match &t {
            Ty::Tuple(ts) if (i as usize) < ts.len() => return ts[i as usize].clone(),
            Ty::Adt(d, args) if !self.c.adts[d].is_enum => {
                let info = self.c.adts[d].clone();
                if let Shape::Tuple(ts) = &info.shape {
                    if let Some(ft) = ts.get(i as usize) {
                        let map = info
                            .generics
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect();
                        return ft.subst(&map);
                    }
                }
            }
            Ty::Unknown => return Ty::Unknown,
            _ => {}
        }
        let tn = self.fmt(bt);
        self.error(format!("no field `{i}` on type `{tn}`"), span);
        Ty::Unknown
    }

    fn index(&mut self, bt: &Ty, it: &Ty, span: Span) -> Ty {
        let mut t = self.strip_refs(bt);
        for _ in 0..8 {
            match self.overloaded_deref(&t, "Deref") {
                Some(u) => t = self.strip_refs(&u),
                None => break,
            }
        }
        let elem = match &t {
            Ty::Adt(d, args) if self.c.is_lang(*d, "Vec") => args[0].clone(),
            Ty::Array(x, _) | Ty::Slice(x) => (**x).clone(),
            Ty::Adt(d, _) if self.c.is_lang(*d, "string") => {
                return Ty::Prim(PrimTy::Str);
            }
            Ty::Prim(PrimTy::Str) => return Ty::Prim(PrimTy::Str),
            Ty::Unknown => return Ty::Unknown,
            _ => {
                let tn = self.fmt(bt);
                self.error(format!("cannot index into a value of type `{tn}`"), span);
                return Ty::Unknown;
            }
        };
        match self.shallow(it) {
            Ty::Adt(d, _) if self.c.is_lang(d, "Range") => Ty::Slice(Box::new(elem)),
            _ => {
                self.coerce(it, &int_ty(PrimTy::Usize), span);
                elem
            }
        }
    }

    // ----- `?` -------------------------------------------------------------------------

    fn try_op(&mut self, t: &Ty, span: Span) -> Ty {
        let ret = self.rets.last().cloned().unwrap_or(Ty::Unit);
        let ret = self.shallow(&ret);
        match self.shallow(t) {
            Ty::Unknown => Ty::Unknown,
            Ty::Adt(d, args) if self.c.is_lang(d, "result") => {
                match &ret {
                    Ty::Adt(rd, rargs) if self.c.is_lang(*rd, "result") => {
                        let (from, to) = (args[1].clone(), rargs[1].clone());
                        if !self.c.infer.try_unify(&from, &to) && !self.has_from(&to, &from) {
                            let (f, t) = (self.fmt(&from), self.fmt(&to));
                            self.error_help(
                                format!("`?` couldn't convert the error to `{t}`"),
                                span,
                                format!("the error type is `{f}`; implement `From<{f}> for {t}`"),
                            );
                        }
                    }
                    Ty::Unknown | Ty::Var(_) => {}
                    other => self.try_outside(other, "result", span),
                }
                args[0].clone()
            }
            Ty::Adt(d, args) if self.c.is_lang(d, "option") => {
                match &ret {
                    Ty::Adt(rd, _) if self.c.is_lang(*rd, "option") => {}
                    Ty::Unknown | Ty::Var(_) => {}
                    other => self.try_outside(other, "option", span),
                }
                args[0].clone()
            }
            other => {
                let tn = self.fmt(&other);
                self.error(format!("the `?` operator can only be applied to `result` or `option`, found `{tn}`"), span);
                Ty::Unknown
            }
        }
    }

    fn try_outside(&mut self, ret: &Ty, kind: &str, span: Span) {
        let r = self.fmt(ret);
        self.error_help(
            format!(
                "the `?` operator on `{kind}` can only be used in a function that returns `{kind}`"
            ),
            span,
            format!("this function returns `{r}`"),
        );
    }

    /// Whether `impl From<from> for to` exists.
    fn has_from(&mut self, to: &Ty, from: &Ty) -> bool {
        let Some(tr) = self.c.r.prelude_type("From") else {
            return false;
        };
        for idx in 0..self.c.impls.len() {
            let Some((t, args)) = self.c.impls[idx].trait_ref.clone() else {
                continue;
            };
            if t != tr || args.len() != 1 {
                continue;
            }
            let (st, map) = self.instantiate_impl(idx);
            let arg = args[0].subst(&map);
            let snapshot = self.c.infer.clone();
            let ok = self.c.infer.unify(&st, to) && self.c.infer.unify(&arg, from);
            self.c.infer = snapshot;
            if ok {
                return true;
            }
        }
        false
    }

    // ----- operators ---------------------------------------------------------------------

    fn is_numeric(&self, t: &Ty) -> bool {
        match self.shallow(t) {
            Ty::Var(v) => self.c.infer.kind(v) != VarKind::General,
            t => t.is_int_prim() || t.is_float_prim(),
        }
    }

    fn is_integral(&self, t: &Ty) -> bool {
        match self.shallow(t) {
            Ty::Var(v) => self.c.infer.kind(v) == VarKind::Int,
            t => t.is_int_prim(),
        }
    }

    fn arith(&mut self, op: BinOp, lt: &Ty, rt: &Ty, span: Span) -> Ty {
        let (l, r) = (self.strip_refs(lt), self.strip_refs(rt));
        if matches!(l, Ty::Unknown) || matches!(r, Ty::Unknown) {
            return Ty::Unknown;
        }
        // String concatenation: String + &str
        if op == BinOp::Add {
            if let Ty::Adt(d, _) = &l {
                if self.c.is_lang(*d, "string") {
                    if !matches!(r, Ty::Prim(PrimTy::Str))
                        && !matches!(&r, Ty::Adt(d2, _) if self.c.is_lang(*d2, "string"))
                    {
                        let f = self.fmt(rt);
                        self.error(format!("cannot add `{f}` to `string`"), span);
                    }
                    return l;
                }
            }
        }
        let sym = op_symbol(op);
        let bitwise = matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor);
        let ok_operand = |s: &Self, t: &Ty| match op {
            BinOp::Shl | BinOp::Shr => s.is_integral(t),
            _ if bitwise => s.is_integral(t) || matches!(s.shallow(t), Ty::Prim(PrimTy::Bool)),
            _ => s.is_numeric(t),
        };
        if matches!(l, Ty::Var(v) if self.c.infer.kind(v) == VarKind::General) {
            self.c.infer.unify(&l, &r);
            return l;
        }
        if !ok_operand(self, &l) {
            let f = self.fmt(&l);
            self.error(
                format!("cannot apply `{sym}` to a value of type `{f}`"),
                span,
            );
            return Ty::Unknown;
        }
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            if !self.is_integral(&r) {
                let f = self.fmt(&r);
                self.error(
                    format!("shift amount must be an integer, found `{f}`"),
                    span,
                );
            }
            return l;
        }
        if !self.c.infer.unify(&l, &r) {
            let (a, b) = (self.fmt(&l), self.fmt(&r));
            self.error_help(
                format!("mismatched types: cannot apply `{sym}` to `{a}` and `{b}`"),
                span,
                "convert one side with `as`",
            );
        }
        l
    }

    fn binary(&mut self, op: BinOp, lt: &Ty, rt: &Ty, span: Span) -> Ty {
        let bool_ty = Ty::Prim(PrimTy::Bool);
        match op {
            BinOp::And | BinOp::Or => {
                for t in [lt, rt] {
                    if !self.c.infer.unify(t, &bool_ty) {
                        let f = self.fmt(t);
                        self.error(
                            format!("`{}` requires `bool` operands, found `{f}`", op_symbol(op)),
                            span,
                        );
                    }
                }
                bool_ty
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let (l, r) = (self.strip_refs(lt), self.strip_refs(rt));
                let str_like = |s: &Self, t: &Ty| {
                    matches!(t, Ty::Prim(PrimTy::Str))
                        || matches!(t, Ty::Adt(d, _) if s.c.is_lang(*d, "string"))
                };
                let direct = self.c.infer.try_unify(lt, rt);
                if !(direct
                    || (str_like(self, &l) && str_like(self, &r))
                    || self.c.infer.unify(&l, &r))
                {
                    let (a, b) = (self.fmt(lt), self.fmt(rt));
                    self.error(
                        format!("mismatched types: cannot compare `{a}` with `{b}`"),
                        span,
                    );
                    return bool_ty;
                }
                let needed = if matches!(op, BinOp::Eq | BinOp::Ne) {
                    "Eq"
                } else {
                    "Ord"
                };
                let l = self.c.infer.zonk(&l);
                let float_ok = matches!(l, Ty::Prim(PrimTy::F32 | PrimTy::F64))
                    || matches!(l, Ty::Var(v) if self.c.infer.kind(v) == VarKind::Float);
                if !float_ok {
                    if let Some(tr) = self.c.r.prelude_type(needed) {
                        self.obligations.push((l, tr, span));
                    }
                }
                bool_ty
            }
            _ => self.arith(op, lt, rt, span),
        }
    }

    fn unary(&mut self, op: UnOp, x: &Expr, span: Span) -> Ty {
        if op == UnOp::Neg && matches!(x.kind, ExprKind::Lit(Lit::Int { .. })) {
            self.negated_lits.insert(x.span);
        }
        let t = self.expr(x);
        match op {
            UnOp::Neg => {
                let s = self.strip_refs(&t);
                let signed = match &s {
                    Ty::Prim(p) => {
                        !matches!(
                            p,
                            PrimTy::U8
                                | PrimTy::U16
                                | PrimTy::U32
                                | PrimTy::U64
                                | PrimTy::U128
                                | PrimTy::Usize
                        ) && self.is_numeric(&s)
                    }
                    Ty::Var(_) => self.is_numeric(&s),
                    Ty::Unknown => true,
                    _ => false,
                };
                if !signed {
                    let f = self.fmt(&t);
                    self.error(format!("cannot negate a value of type `{f}`"), span);
                }
                s
            }
            UnOp::Not => {
                let s = self.strip_refs(&t);
                if !(self.is_integral(&s) || matches!(s, Ty::Prim(PrimTy::Bool) | Ty::Unknown)) {
                    let f = self.fmt(&t);
                    self.error(format!("cannot apply `!` to a value of type `{f}`"), span);
                }
                s
            }
            UnOp::Deref => match self.shallow(&t) {
                Ty::Ref(_, i) => *i,
                Ty::Ptr(_, i) => {
                    self.require_unsafe("dereferencing a raw pointer", span);
                    *i
                }
                ot @ Ty::Adt(..) if self.overloaded_deref(&ot, "Deref").is_some() => {
                    self.overloaded_deref(&ot, "Deref").unwrap()
                }
                Ty::Adt(d, args) if self.c.is_lang(d, "Box") => args[0].clone(),
                Ty::Unknown => Ty::Unknown,
                other => {
                    let f = self.fmt(&other);
                    self.error(format!("type `{f}` cannot be dereferenced"), span);
                    Ty::Unknown
                }
            },
            UnOp::Ref => Ty::Ref(false, Box::new(t)),
            UnOp::RefMut => {
                self.check_place_mut(x, "borrow");
                Ty::Ref(true, Box::new(t))
            }
            UnOp::Recv => self.recv_elem(&t, x.span),
        }
    }

    fn check_cast(&mut self, from: &Ty, to: &Ty, span: Span) {
        let f = self.shallow(from);
        let t = self.shallow(to);
        let num = |s: &Self, x: &Ty| s.is_numeric(x);
        let ok = match (&f, &t) {
            (Ty::Unknown, _) | (_, Ty::Unknown) => true,
            _ if num(self, &f) && num(self, &t) => true,
            (Ty::Prim(PrimTy::Char | PrimTy::Bool), _) if self.is_integral(&t) => true,
            (Ty::Prim(PrimTy::U8), Ty::Prim(PrimTy::Char)) => true,
            (Ty::Var(v), Ty::Prim(PrimTy::Char)) if self.c.infer.kind(*v) == VarKind::Int => {
                self.c.infer.unify(&f, &Ty::Prim(PrimTy::U8));
                true
            }
            (Ty::Ref(..) | Ty::Ptr(..), Ty::Ptr(..)) => true,
            (Ty::Ptr(..), _) if self.is_integral(&t) => true,
            (Ty::Prim(_) | Ty::Var(_), Ty::Ptr(..)) => true,
            _ => self.c.infer.can_unify(&f, &t),
        };
        if !ok {
            let (a, b) = (self.fmt(from), self.fmt(to));
            self.error(format!("invalid cast from `{a}` to `{b}`"), span);
        }
    }

    // ----- struct literals -----------------------------------------------------------------

    fn struct_lit(
        &mut self,
        path: &Path,
        fields: &[FieldInit],
        base: Option<&Expr>,
        span: Span,
    ) -> Ty {
        let res = self.c.r.path(path.span);
        let (adt, args, shape, what) = match res {
            Res::Def(d) if self.c.r.def(d).kind == DefKind::Struct => {
                let explicit = self.seg_args(path, path.segments.len() - 1);
                let args = self.fresh_args(d, explicit);
                (
                    d,
                    args,
                    self.c.adts[&d].shape.clone(),
                    self.c.r.def(d).name.clone(),
                )
            }
            Res::Def(d) if matches!(self.c.r.def(d).kind, DefKind::Variant { .. }) => {
                let (adt, shape) = self.variant_info(d);
                let args = self.fresh_args(adt, vec![]);
                (adt, args, shape, self.c.r.def(d).name.clone())
            }
            Res::SelfTy => match self.self_ty.clone().map(|t| self.shallow(&t)) {
                Some(Ty::Adt(d, args)) if !self.c.adts[&d].is_enum => {
                    (d, args, self.c.adts[&d].shape.clone(), "Self".to_string())
                }
                _ => return self.lit_fallback(fields, base),
            },
            Res::Assoc { base: b, name } => {
                let enum_ty = match *b {
                    Res::SelfTy => self.self_ty.clone(),
                    _ => None,
                };
                match enum_ty.map(|t| self.shallow(&t)) {
                    Some(Ty::Adt(d, args)) if self.c.adts[&d].is_enum => {
                        let vs = self.c.adts[&d].variants.clone();
                        match vs.into_iter().find(|(v, _)| self.c.r.def(*v).name == name) {
                            Some((_, shape)) => (d, args, shape, name.clone()),
                            None => return self.lit_fallback(fields, base),
                        }
                    }
                    _ => return self.lit_fallback(fields, base),
                }
            }
            _ => return self.lit_fallback(fields, base),
        };
        let me = Ty::Adt(adt, args.clone());
        let Shape::Named(decl) = shape else {
            self.error(
                format!("`{what}` has no named fields; use `{what}(...)`"),
                span,
            );
            self.lit_fallback(fields, base);
            return me;
        };
        let map: HashMap<String, Ty> = self.c.adts[&adt]
            .generics
            .iter()
            .cloned()
            .zip(args)
            .collect();
        let mut seen: Vec<&str> = Vec::new();
        for f in fields {
            let vt = self.expr(&f.value);
            if seen.contains(&f.name.name.as_str()) {
                self.error(
                    format!("field `{}` specified more than once", f.name.name),
                    f.name.span,
                );
                continue;
            }
            seen.push(&f.name.name);
            match decl.iter().find(|(n, _)| *n == f.name.name) {
                Some((_, ft)) => {
                    let ft = ft.subst(&map);
                    self.coerce(&vt, &ft, f.value.span);
                }
                None => self.error(
                    format!("`{what}` has no field named `{}`", f.name.name),
                    f.name.span,
                ),
            }
        }
        match base {
            Some(b) => {
                let bt = self.expr(b);
                self.coerce(&bt, &me, b.span);
            }
            None => {
                let missing: Vec<String> = decl
                    .iter()
                    .filter(|(n, _)| !seen.contains(&n.as_str()))
                    .map(|(n, _)| format!("`{n}`"))
                    .collect();
                if !missing.is_empty() {
                    self.error(
                        format!(
                            "missing field{} {} in initializer of `{what}`",
                            if missing.len() == 1 { "" } else { "s" },
                            missing.join(", ")
                        ),
                        span,
                    );
                }
            }
        }
        me
    }

    fn lit_fallback(&mut self, fields: &[FieldInit], base: Option<&Expr>) -> Ty {
        for f in fields {
            self.expr(&f.value);
        }
        if let Some(b) = base {
            self.expr(b);
        }
        Ty::Unknown
    }

    // ----- patterns ------------------------------------------------------------------------

    /// Binds pattern `p` against a value of type `ty`. `mode` is the default
    /// binding mode (`Some(mutable)` when matching through a reference, which
    /// makes bindings references, as in Rust's match ergonomics).
    fn bind_pat(&mut self, p: &Pattern, ty: &Ty, mode: Option<bool>) {
        let t = self.shallow(ty);
        // Match ergonomics: destructuring patterns see through references.
        let destructures = matches!(
            p.kind,
            PatternKind::Tuple(_)
                | PatternKind::TupleStruct(..)
                | PatternKind::Struct { .. }
                | PatternKind::Path(_)
                | PatternKind::Slice(_)
                | PatternKind::Lit(_)
                | PatternKind::Range { .. }
        );
        if destructures {
            if let Ty::Ref(m, inner) = &t {
                let mode = Some(mode.unwrap_or(*m) && *m);
                return self.bind_pat(p, inner, mode);
            }
        }
        match &p.kind {
            PatternKind::Wild | PatternKind::Rest => {}
            PatternKind::Ident { name, sub, .. } => {
                if let Some(&id) = self.c.r.bindings.get(&name.span) {
                    let bt = match mode {
                        Some(m) => Ty::Ref(m, Box::new(t.clone())),
                        None => t.clone(),
                    };
                    self.locals.insert(id, bt);
                } else {
                    // A unit variant / unit struct / const used as a pattern
                    // (match ergonomics apply, as for `Path` patterns).
                    let mut inner = t.clone();
                    while let Ty::Ref(_, x) = inner {
                        inner = self.shallow(&x);
                    }
                    self.path_pat(name.span, &inner, p.span);
                }
                if let Some(s) = sub {
                    self.bind_pat(s, &t, mode);
                }
            }
            PatternKind::Lit(l) => {
                let lt = self.lit_ty(l);
                let lt = match (&lt, &t) {
                    (Ty::Ref(_, x), Ty::Prim(PrimTy::Str)) => (**x).clone(),
                    (Ty::Ref(_, _), Ty::Adt(d, _)) if self.c.is_lang(*d, "string") => t.clone(),
                    _ => lt,
                };
                self.pat_unify(&lt, &t, p.span);
            }
            PatternKind::Range { lo, hi, .. } => {
                self.bind_pat(lo, &t, None);
                self.bind_pat(hi, &t, None);
                if !(self.is_integral(&t)
                    || matches!(self.shallow(&t), Ty::Prim(PrimTy::Char) | Ty::Unknown))
                {
                    let f = self.fmt(&t);
                    self.error(
                        format!("range patterns require integer or `char` values, found `{f}`"),
                        p.span,
                    );
                }
            }
            PatternKind::Tuple(ps) => {
                let has_rest = ps.iter().any(|x| matches!(x.kind, PatternKind::Rest));
                match &t {
                    Ty::Tuple(ts) if has_rest || ts.len() == ps.len() => {
                        let ts = ts.clone();
                        let rest_at = ps
                            .iter()
                            .position(|x| matches!(x.kind, PatternKind::Rest))
                            .unwrap_or(ps.len());
                        let after = ps.len() - rest_at - usize::from(has_rest);
                        for (i, sp) in ps.iter().enumerate() {
                            #[allow(clippy::comparison_chain)]
                            let ti = if i < rest_at {
                                Some(i)
                            } else if i > rest_at {
                                ts.len().checked_sub(after - (i - rest_at - 1))
                            } else {
                                None
                            };
                            if let Some(ti) = ti {
                                let et = ts.get(ti).cloned().unwrap_or(Ty::Unknown);
                                self.bind_pat(sp, &et, mode);
                            }
                        }
                    }
                    Ty::Unknown => ps.iter().for_each(|x| self.bind_pat(x, &Ty::Unknown, mode)),
                    // `()` matches the unit value.
                    _ if ps.is_empty() => self.pat_unify(&Ty::Unit, &t, p.span),
                    _ if !has_rest => {
                        let vars: Vec<Ty> = ps.iter().map(|_| self.fresh()).collect();
                        let tup = Ty::Tuple(vars.clone());
                        self.pat_unify(&tup, &t, p.span);
                        for (sp, v) in ps.iter().zip(vars) {
                            self.bind_pat(sp, &v, mode);
                        }
                    }
                    _ => {
                        let f = self.fmt(&t);
                        self.error(
                            format!("mismatched types: expected `{f}`, found a tuple pattern"),
                            p.span,
                        );
                    }
                }
            }
            PatternKind::Slice(ps) => {
                let elem = match &t {
                    Ty::Array(x, _) | Ty::Slice(x) => (**x).clone(),
                    Ty::Adt(d, a) if self.c.is_lang(*d, "Vec") => a[0].clone(),
                    Ty::Unknown => Ty::Unknown,
                    _ => {
                        let f = self.fmt(&t);
                        self.error(
                            format!("mismatched types: expected `{f}`, found a slice pattern"),
                            p.span,
                        );
                        Ty::Unknown
                    }
                };
                for sp in ps {
                    self.bind_pat(sp, &elem, mode);
                }
            }
            PatternKind::Path(path) => self.path_pat(path.span, &t, p.span),
            PatternKind::TupleStruct(path, ps) => {
                let Some((adt, shape, name)) = self.pat_ctor(path.span, &t, p.span) else {
                    ps.iter().for_each(|x| self.bind_pat(x, &Ty::Unknown, mode));
                    return;
                };
                let Ty::Adt(_, args) = self.shallow(&t) else {
                    return;
                };
                let map: HashMap<String, Ty> = self.c.adts[&adt]
                    .generics
                    .iter()
                    .cloned()
                    .zip(args)
                    .collect();
                let fields: Vec<Ty> = match &shape {
                    Shape::Tuple(ts) => ts.iter().map(|x| x.subst(&map)).collect(),
                    _ => {
                        self.error(
                            format!("`{name}` is not a tuple variant or tuple struct"),
                            p.span,
                        );
                        ps.iter().for_each(|x| self.bind_pat(x, &Ty::Unknown, mode));
                        return;
                    }
                };
                let has_rest = ps.iter().any(|x| matches!(x.kind, PatternKind::Rest));
                if !has_rest && ps.len() != fields.len() {
                    self.error(
                        format!(
                            "this pattern has {} field{}, but `{name}` has {}",
                            ps.len(),
                            if ps.len() == 1 { "" } else { "s" },
                            fields.len()
                        ),
                        p.span,
                    );
                }
                for (i, sp) in ps.iter().enumerate() {
                    let ft = fields.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pat(sp, &ft, mode);
                }
            }
            PatternKind::Struct { path, fields, rest } => {
                let Some((adt, shape, name)) = self.pat_ctor(path.span, &t, p.span) else {
                    fields
                        .iter()
                        .for_each(|f| self.bind_pat(&f.pat, &Ty::Unknown, mode));
                    return;
                };
                let Ty::Adt(_, args) = self.shallow(&t) else {
                    return;
                };
                let map: HashMap<String, Ty> = self.c.adts[&adt]
                    .generics
                    .iter()
                    .cloned()
                    .zip(args)
                    .collect();
                let Shape::Named(decl) = shape else {
                    self.error(format!("`{name}` has no named fields"), p.span);
                    return;
                };
                for f in fields {
                    match decl.iter().find(|(n, _)| *n == f.name.name) {
                        Some((_, ft)) => {
                            let ft = ft.subst(&map);
                            self.bind_pat(&f.pat, &ft, mode);
                        }
                        None => {
                            self.error(
                                format!("`{name}` has no field named `{}`", f.name.name),
                                f.name.span,
                            );
                            self.bind_pat(&f.pat, &Ty::Unknown, mode);
                        }
                    }
                }
                if !rest {
                    let missing: Vec<String> = decl
                        .iter()
                        .filter(|(n, _)| !fields.iter().any(|f| f.name.name == *n))
                        .map(|(n, _)| format!("`{n}`"))
                        .collect();
                    if !missing.is_empty() {
                        self.error_help(
                            format!(
                                "pattern does not mention field{} {}",
                                if missing.len() == 1 { "" } else { "s" },
                                missing.join(", ")
                            ),
                            p.span,
                            "add the missing fields or `..` to ignore them",
                        );
                    }
                }
            }
            PatternKind::Ref { mutable, inner } => match &t {
                Ty::Ref(m, x) if m == mutable => self.bind_pat(inner, x, None),
                Ty::Unknown => self.bind_pat(inner, &Ty::Unknown, None),
                Ty::Var(_) => {
                    let v = self.fresh();
                    self.c
                        .infer
                        .unify(&t, &Ty::Ref(*mutable, Box::new(v.clone())));
                    self.bind_pat(inner, &v, None);
                }
                _ => {
                    let f = self.fmt(&t);
                    self.error(
                        format!("mismatched types: expected `{f}`, found a reference pattern"),
                        p.span,
                    );
                }
            },
            PatternKind::Or(alts) => {
                for a in alts {
                    self.bind_pat(a, &t, mode);
                }
            }
        }
    }

    fn pat_unify(&mut self, pat_ty: &Ty, t: &Ty, span: Span) {
        if !self.c.infer.unify(pat_ty, t) {
            let (e, f) = (self.fmt(t), self.fmt(pat_ty));
            self.error(
                format!("mismatched types: expected `{e}`, found `{f}`"),
                span,
            );
        }
    }

    /// The constructor a pattern path names, unified with the scrutinee type.
    fn pat_ctor(&mut self, path_span: Span, t: &Ty, span: Span) -> Option<(DefId, Shape, String)> {
        let res = self.c.r.path(path_span);
        let (adt, shape, name) = match res {
            Res::Def(d) => match self.c.r.def(d).kind {
                DefKind::Variant { .. } => {
                    let (adt, shape) = self.variant_info(d);
                    (adt, shape, self.c.r.def(d).name.clone())
                }
                DefKind::Struct => (
                    d,
                    self.c.adts[&d].shape.clone(),
                    self.c.r.def(d).name.clone(),
                ),
                _ => return None,
            },
            Res::Assoc { base, name } if *base == Res::SelfTy => {
                let st = self.self_ty.clone().map(|x| self.shallow(&x));
                let Some(Ty::Adt(d, _)) = st else { return None };
                let vs = self.c.adts.get(&d)?.variants.clone();
                let (_, shape) = vs
                    .into_iter()
                    .find(|(v, _)| self.c.r.def(*v).name == name)?;
                (d, shape, name)
            }
            Res::SelfTy => {
                let st = self.self_ty.clone().map(|x| self.shallow(&x));
                let Some(Ty::Adt(d, _)) = st else { return None };
                (d, self.c.adts[&d].shape.clone(), "Self".into())
            }
            _ => return None,
        };
        let args = self.fresh_args(adt, vec![]);
        let at = Ty::Adt(adt, args);
        if !self.c.infer.unify(&at, t) {
            let (e, f) = (self.fmt(t), self.fmt(&at));
            self.error(
                format!("mismatched types: expected `{e}`, found `{f}`"),
                span,
            );
            return None;
        }
        Some((adt, shape, name))
    }

    fn path_pat(&mut self, path_span: Span, t: &Ty, span: Span) {
        match self.c.r.path(path_span) {
            Res::Def(d) if self.c.r.def(d).kind == DefKind::Const => {
                if let Some(ItemKind::Const { ty, .. }) = self.c.r.def(d).item.map(|i| &i.kind) {
                    let ty = ty.clone();
                    let ct = self.c.lower_ty(&ty, None, false);
                    self.pat_unify(&ct, t, span);
                }
            }
            _ => {
                if let Some((_, shape, name)) = self.pat_ctor(path_span, t, span) {
                    if !matches!(shape, Shape::Unit) {
                        self.error_help(
                            format!("`{name}` has fields but the pattern has none"),
                            span,
                            format!("use `{name}(..)` or `{name} {{ .. }}`"),
                        );
                    }
                }
            }
        }
    }

    // ----- match -----------------------------------------------------------------------------

    fn match_expr(&mut self, scrut: &Expr, arms: &[MatchArm], value: bool, span: Span) -> Ty {
        let st = self.expr(scrut);
        let mut result: Option<Ty> = None;
        let mut all_never = !arms.is_empty();
        for arm in arms {
            self.bind_pat(&arm.pat, &st, None);
            if let Some(g) = &arm.guard {
                let gt = self.expr(g);
                self.coerce(&gt, &Ty::Prim(PrimTy::Bool), g.span);
            }
            let bt = self.expr_v(&arm.body, value);
            let never = matches!(self.shallow(&bt), Ty::Never);
            all_never &= never;
            if value && !never {
                result = Some(match result {
                    None => bt,
                    Some(r) => self.join(&r, &bt, arm.body.span),
                });
            }
        }
        self.check_exhaustive(&st, arms, span);
        if value {
            result.unwrap_or(if all_never { Ty::Never } else { Ty::Unit })
        } else if all_never {
            Ty::Never
        } else {
            Ty::Unit
        }
    }

    // ----- exhaustiveness ---------------------------------------------------------
    //
    // The usefulness algorithm (Maranget, "Warnings for pattern matching"):
    // a match is exhaustive when a wildcard row is not useful after its arms.
    // Enum variants, `bool`, tuples, and structs are constructors; values of
    // other types (integers, strings) are only covered by wildcards.

    fn x_fields(&self, t: &Ty, variant: Option<DefId>) -> Vec<(String, Ty)> {
        let Ty::Adt(d, args) = t else { return vec![] };
        let Some(info) = self.c.adts.get(d) else {
            return vec![];
        };
        let map: HashMap<String, Ty> = info
            .generics
            .iter()
            .cloned()
            .zip(args.iter().cloned())
            .collect();
        let shape = match variant {
            None => &info.shape,
            Some(v) => match info.variants.iter().find(|(id, _)| *id == v) {
                Some((_, sh)) => sh,
                None => return vec![],
            },
        };
        match shape {
            crate::collect::Shape::Named(fs) => {
                fs.iter().map(|(n, t)| (n.clone(), t.subst(&map))).collect()
            }
            crate::collect::Shape::Tuple(ts) => ts
                .iter()
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.subst(&map)))
                .collect(),
            crate::collect::Shape::Unit => vec![],
        }
    }

    /// All constructors of `t` with their field types, or `None` when its
    /// values can't be enumerated.
    fn x_ctors(&self, t: &Ty) -> Option<Vec<(XCtor, Vec<Ty>)>> {
        let t = self.c.infer.zonk(&self.strip_refs(t));
        match &t {
            Ty::Prim(PrimTy::Bool) => Some(vec![
                (XCtor::Bool(true), vec![]),
                (XCtor::Bool(false), vec![]),
            ]),
            Ty::Unit => Some(vec![(XCtor::Single, vec![])]),
            Ty::Tuple(ts) => Some(vec![(XCtor::Single, ts.clone())]),
            Ty::Array(e, n) => Some(vec![(XCtor::Len(*n as usize), vec![(**e).clone(); *n as usize])]),
            Ty::Slice(e) => Some(slice_ctors(e)),
            Ty::Adt(d, a) if self.c.is_lang(*d, "Vec") && !a.is_empty() => Some(slice_ctors(&a[0])),
            Ty::Adt(d, _) => {
                let info = self.c.adts.get(d)?;
                if info.is_enum {
                    let vs: Vec<DefId> = info.variants.iter().map(|(v, _)| *v).collect();
                    Some(
                        vs.into_iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let fs = self
                                    .x_fields(&t, Some(v))
                                    .into_iter()
                                    .map(|(_, t)| t)
                                    .collect();
                                (XCtor::Variant(i), fs)
                            })
                            .collect(),
                    )
                } else {
                    let fs = self
                        .x_fields(&t, None)
                        .into_iter()
                        .map(|(_, t)| t)
                        .collect();
                    Some(vec![(XCtor::Single, fs)])
                }
            }
            _ => None,
        }
    }

    /// The constructor named by a pattern path, with its named fields.
    fn x_ctor_of(&self, span: Span, t: &Ty) -> Option<(XCtor, Vec<(String, Ty)>)> {
        let t = self.c.infer.zonk(&self.strip_refs(t));
        match self.c.r.path(span) {
            Res::Def(d) => match self.c.r.def(d).kind {
                DefKind::Variant { index, .. } => {
                    Some((XCtor::Variant(index), self.x_fields(&t, Some(d))))
                }
                DefKind::Struct => Some((XCtor::Single, self.x_fields(&t, None))),
                _ => None,
            },
            Res::Assoc { name, .. } => {
                let Ty::Adt(d, _) = &t else { return None };
                let info = self.c.adts.get(d)?;
                let (i, (v, _)) = info
                    .variants
                    .iter()
                    .enumerate()
                    .find(|(_, (v, _))| self.c.r.def(*v).name == name)?;
                Some((XCtor::Variant(i), self.x_fields(&t, Some(*v))))
            }
            _ => None,
        }
    }

    /// Positional sub-patterns, expanding `..` to wildcards.
    fn x_positional(&self, ps: &[Pattern], tys: &[Ty]) -> Vec<XPat> {
        let n = tys.len();
        let rest = ps.iter().position(|x| matches!(x.kind, PatternKind::Rest));
        let ty = |i: usize| tys.get(i).cloned().unwrap_or(Ty::Unknown);
        let mut out = vec![XPat::Wild; n];
        match rest {
            Some(r) => {
                for (i, p) in ps[..r].iter().enumerate() {
                    if i < n {
                        out[i] = self.x_lower(p, &ty(i));
                    }
                }
                let after = &ps[r + 1..];
                for (k, p) in after.iter().enumerate() {
                    if let Some(i) = (n + k).checked_sub(after.len()) {
                        out[i] = self.x_lower(p, &ty(i));
                    }
                }
            }
            None => {
                for (i, p) in ps.iter().enumerate().take(n) {
                    out[i] = self.x_lower(p, &ty(i));
                }
            }
        }
        out
    }

    fn x_lower(&self, p: &Pattern, t: &Ty) -> XPat {
        match &p.kind {
            PatternKind::Wild | PatternKind::Rest => XPat::Wild,
            PatternKind::Ident { name, sub, .. } => {
                if self.c.r.bindings.contains_key(&name.span) {
                    return match sub {
                        Some(s) => self.x_lower(s, t),
                        None => XPat::Wild,
                    };
                }
                match self.x_ctor_of(name.span, t) {
                    Some((k, fs)) => XPat::Ctor(k, vec![XPat::Wild; fs.len()]),
                    None => XPat::Opaque,
                }
            }
            PatternKind::Ref { inner, .. } => self.x_lower(inner, t),
            PatternKind::Or(ps) => XPat::Or(ps.iter().map(|x| self.x_lower(x, t)).collect()),
            PatternKind::Lit(Lit::Bool(b)) => XPat::Ctor(XCtor::Bool(*b), vec![]),
            PatternKind::Slice(ps) => {
                let elem = match self.c.infer.zonk(&self.strip_refs(t)) {
                    Ty::Array(e, _) | Ty::Slice(e) => *e,
                    Ty::Adt(_, a) if !a.is_empty() => a[0].clone(),
                    _ => Ty::Unknown,
                };
                let lower = |x: &Pattern| self.x_lower(x, &elem);
                match ps.iter().position(|x| matches!(x.kind, PatternKind::Rest)) {
                    None => XPat::Ctor(XCtor::Len(ps.len()), ps.iter().map(lower).collect()),
                    Some(r) => {
                        let pre: Vec<XPat> = ps[..r].iter().map(lower).collect();
                        let suf: Vec<XPat> = ps[r + 1..].iter().map(lower).collect();
                        let max = slice_max();
                        let fill = |n: usize| {
                            let mut v = pre.clone();
                            v.extend(std::iter::repeat_n(XPat::Wild, n - pre.len() - suf.len()));
                            v.extend(suf.iter().cloned());
                            v
                        };
                        let mut alts: Vec<XPat> = (pre.len() + suf.len()..=max)
                            .map(|n| XPat::Ctor(XCtor::Len(n), fill(n)))
                            .collect();
                        alts.push(XPat::Ctor(XCtor::Long, fill(max + 1)));
                        XPat::Or(alts)
                    }
                }
            }
            PatternKind::Tuple(ps) => {
                let tys = match self.c.infer.zonk(&self.strip_refs(t)) {
                    Ty::Tuple(ts) => ts,
                    _ => vec![Ty::Unknown; ps.len()],
                };
                XPat::Ctor(XCtor::Single, self.x_positional(ps, &tys))
            }
            PatternKind::Path(path) => match self.x_ctor_of(path.span, t) {
                Some((k, fs)) => XPat::Ctor(k, vec![XPat::Wild; fs.len()]),
                None => XPat::Opaque,
            },
            PatternKind::TupleStruct(path, ps) => match self.x_ctor_of(path.span, t) {
                Some((k, fs)) => {
                    let tys: Vec<Ty> = fs.into_iter().map(|(_, t)| t).collect();
                    XPat::Ctor(k, self.x_positional(ps, &tys))
                }
                None => XPat::Opaque,
            },
            PatternKind::Struct { path, fields, .. } => match self.x_ctor_of(path.span, t) {
                Some((k, fs)) => XPat::Ctor(
                    k,
                    fs.iter()
                        .map(|(n, ft)| match fields.iter().find(|f| f.name.name == *n) {
                            Some(f) => self.x_lower(&f.pat, ft),
                            None => XPat::Wild,
                        })
                        .collect(),
                ),
                None => XPat::Opaque,
            },
            _ => XPat::Opaque,
        }
    }

    /// Whether the pattern row `q` matches some value that no row of `rows` matches.
    fn x_useful(&self, rows: &[Vec<XPat>], q: &[XPat], tys: &[Ty]) -> bool {
        if q.is_empty() {
            return rows.is_empty();
        }
        // Expand or-patterns in the first column.
        let mut flat: Vec<Vec<XPat>> = vec![];
        let mut stack: Vec<Vec<XPat>> = rows.iter().rev().cloned().collect();
        while let Some(r) = stack.pop() {
            match &r[0] {
                XPat::Or(alts) => {
                    for a in alts.iter().rev() {
                        let mut v = vec![a.clone()];
                        v.extend_from_slice(&r[1..]);
                        stack.push(v);
                    }
                }
                _ => flat.push(r),
            }
        }
        let specialize = |k: XCtor, arity: usize| -> Vec<Vec<XPat>> {
            flat.iter()
                .filter_map(|r| {
                    let head = match &r[0] {
                        XPat::Ctor(k2, args) if *k2 == k => args.clone(),
                        XPat::Wild => vec![XPat::Wild; arity],
                        _ => return None,
                    };
                    let mut v = head;
                    v.extend_from_slice(&r[1..]);
                    Some(v)
                })
                .collect()
        };
        let ctor_tys = |k: XCtor, arity: usize| -> Vec<Ty> {
            self.x_ctors(&tys[0])
                .and_then(|cs| cs.into_iter().find(|(c, _)| *c == k).map(|(_, t)| t))
                .unwrap_or_else(|| vec![Ty::Unknown; arity])
        };
        match &q[0] {
            XPat::Or(alts) => alts.iter().any(|a| {
                let mut v = vec![a.clone()];
                v.extend_from_slice(&q[1..]);
                self.x_useful(&flat, &v, tys)
            }),
            XPat::Ctor(k, args) => {
                let mut nq = args.clone();
                nq.extend_from_slice(&q[1..]);
                let mut nt = ctor_tys(*k, args.len());
                nt.extend_from_slice(&tys[1..]);
                self.x_useful(&specialize(*k, args.len()), &nq, &nt)
            }
            XPat::Wild | XPat::Opaque => {
                let seen: Vec<XCtor> = flat
                    .iter()
                    .filter_map(|r| match &r[0] {
                        XPat::Ctor(k, _) => Some(*k),
                        _ => None,
                    })
                    .collect();
                match self.x_ctors(&tys[0]) {
                    Some(all) if all.iter().all(|(k, _)| seen.contains(k)) => {
                        all.iter().any(|(k, ftys)| {
                            let mut nq = vec![XPat::Wild; ftys.len()];
                            nq.extend_from_slice(&q[1..]);
                            let mut nt = ftys.clone();
                            nt.extend_from_slice(&tys[1..]);
                            self.x_useful(&specialize(*k, ftys.len()), &nq, &nt)
                        })
                    }
                    _ => {
                        let default: Vec<Vec<XPat>> = flat
                            .iter()
                            .filter(|r| matches!(r[0], XPat::Wild))
                            .map(|r| r[1..].to_vec())
                            .collect();
                        self.x_useful(&default, &q[1..], &tys[1..])
                    }
                }
            }
        }
    }

    fn check_exhaustive(&mut self, st: &Ty, arms: &[MatchArm], span: Span) {
        let mut max = arms.iter().map(|a| pattern_slice_len(&a.pat)).max().unwrap_or(0);
        if let Ty::Array(_, n) = self.c.infer.zonk(&self.strip_refs(st)) {
            max = max.max(n as usize);
        }
        SLICE_MAX.with(|c| c.set(max));
        let rows: Vec<Vec<XPat>> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| vec![self.x_lower(&a.pat, st)])
            .collect();
        let tys = [st.clone()];
        if !self.x_useful(&rows, &[XPat::Wild], &tys) {
            return;
        }
        let t = self.c.infer.zonk(&self.strip_refs(st));
        if matches!(t, Ty::Unknown | Ty::Never) {
            return;
        }
        // Name the top-level constructors that are not fully covered.
        let names: Option<Vec<String>> = match &t {
            Ty::Adt(d, _) if self.c.adts.get(d).is_some_and(|i| i.is_enum) => Some(
                self.c.adts[d]
                    .variants
                    .iter()
                    .map(|(v, _)| self.c.r.def(*v).name.clone())
                    .collect(),
            ),
            Ty::Prim(PrimTy::Bool) => Some(vec!["true".into(), "false".into()]),
            _ => None,
        };
        let missing: Vec<String> = match (names, self.x_ctors(&t)) {
            (Some(names), Some(cs)) => cs
                .iter()
                .zip(names)
                .filter(|((k, ftys), _)| {
                    let q = vec![XPat::Ctor(*k, vec![XPat::Wild; ftys.len()])];
                    self.x_useful(&rows, &q, &tys)
                })
                .map(|(_, n)| format!("`{n}`"))
                .collect(),
            _ => vec![],
        };
        if missing.is_empty() {
            let f = self.fmt(&t);
            self.error_help(
                format!("non-exhaustive match: not all values of `{f}` are covered"),
                span,
                "add a `_ => ...` arm",
            );
        } else {
            self.error_help(
                format!("non-exhaustive match: {} not covered", missing.join(", ")),
                span,
                "add arms for the missing cases or a `_ => ...` arm",
            );
        }
    }
}

/// The constructors of a slice (or `Vec`) of `e`: every length up to
/// [`SLICE_MAX`], then all longer ones together.
fn slice_ctors(e: &Ty) -> Vec<(XCtor, Vec<Ty>)> {
    let max = slice_max();
    let mut v: Vec<(XCtor, Vec<Ty>)> = (0..=max).map(|n| (XCtor::Len(n), vec![e.clone(); n])).collect();
    v.push((XCtor::Long, vec![e.clone(); max + 1]));
    v
}

/// A pattern simplified for exhaustiveness checking.
#[derive(Clone, Debug)]
enum XPat {
    Wild,
    Ctor(XCtor, Vec<XPat>),
    Or(Vec<XPat>),
    /// A literal or range of a type whose values aren't enumerated.
    Opaque,
}

/// A constructor: an enum variant (by index), a `bool`, or the only
/// constructor of a struct or tuple.
#[derive(Clone, Copy, Debug, PartialEq)]
enum XCtor {
    Variant(usize),
    Bool(bool),
    Single,
    /// A slice of exactly this length.
    Len(usize),
    /// A slice longer than every fixed length the match mentions
    /// ([`SLICE_MAX`]); its arity is `SLICE_MAX + 1`, with prefixes at the
    /// start and suffixes at the end.
    Long,
}

thread_local! {
    /// The longest length a slice pattern of the current match can
    /// distinguish (fixed patterns' lengths, `[a, .., b]`'s prefix plus
    /// suffix, and array lengths). Longer slices are all alike: [`XCtor::Long`].
    static SLICE_MAX: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn slice_max() -> usize {
    SLICE_MAX.with(|c| c.get())
}

/// The longest length `p` distinguishes (see [`SLICE_MAX`]).
fn pattern_slice_len(p: &Pattern) -> usize {
    match &p.kind {
        PatternKind::Slice(ps) => {
            let own = ps
                .iter()
                .filter(|x| !matches!(x.kind, PatternKind::Rest))
                .count();
            ps.iter().map(pattern_slice_len).fold(own, usize::max)
        }
        PatternKind::Tuple(ps) | PatternKind::TupleStruct(_, ps) | PatternKind::Or(ps) => {
            ps.iter().map(pattern_slice_len).max().unwrap_or(0)
        }
        PatternKind::Struct { fields, .. } => fields
            .iter()
            .map(|f| pattern_slice_len(&f.pat))
            .max()
            .unwrap_or(0),
        PatternKind::Ref { inner, .. } => pattern_slice_len(inner),
        PatternKind::Ident { sub: Some(s), .. } => pattern_slice_len(s),
        _ => 0,
    }
}

fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}
