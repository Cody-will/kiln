//! Function body checking: inference, coercions, explicit returns, patterns,
//! methods, traits, `?`, channels, and mutability.

use crate::collect::{Checker, Shape};
use crate::ty::{Ty, VarKind};
use kiln_lexer::Span;
use kiln_parser::ast::*;
use kiln_resolve::{Builtin, DefId, DefKind, LocalId, PrimTy, Res};
use std::collections::HashMap;

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
    /// Index into `Resolved::fns` of the callee, when it is a declared function.
    fn_idx: Option<usize>,
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
    /// Trait bounds in scope (`(param, trait)`).
    pub bounds: Vec<(String, DefId)>,
}

/// Per-function checking state.
pub struct Body<'c, 'r, 'a> {
    c: &'c mut Checker<'r, 'a>,
    locals: HashMap<LocalId, Ty>,
    /// Return types of the enclosing function and closures (innermost last).
    rets: Vec<Ty>,
    loops: Vec<LoopCx>,
    bounds: Vec<(String, DefId)>,
    self_ty: Option<Ty>,
    tys: HashMap<Span, Ty>,
    obligations: Vec<(Ty, DefId, Span)>,
    declared: Vec<(LocalId, Span)>,
    methods: HashMap<Span, MethodInfo>,
    callees: HashMap<Span, usize>,
    /// ADTs whose auto-trait impls are being computed (cycle guard).
    auto_stack: Vec<DefId>,
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
            self_ty: None,
            tys: HashMap::new(),
            obligations: vec![],
            declared: vec![],
            methods: HashMap::new(),
            callees: HashMap::new(),
            auto_stack: vec![],
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
            if f.is_prelude || f.decl.body.is_none() {
                continue;
            }
            self.check_fn(i);
        }
        self.check_main();
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
            Ty::Adt(d, args) if self.is_lang(*d, "Result") => args[0] == Ty::Unit,
            _ => false,
        };
        if !ok {
            let found = self.fmt_ty(&sig.ret);
            self.error_help(
                format!("`main` has invalid return type `{found}`"),
                span,
                "`main` must return `()` or `Result<(), E>`",
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
            self_ty: self_ty.clone(),
            tys: HashMap::new(),
            obligations: Vec::new(),
            declared: Vec::new(),
            methods: HashMap::new(),
            callees: HashMap::new(),
            auto_stack: Vec::new(),
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
            locals: self.locals.iter().map(|(k, v)| (*k, z(self.c, v))).collect(),
            tys: self.tys.iter().map(|(k, v)| (*k, z(self.c, v))).collect(),
            methods: self
                .methods
                .iter()
                .map(|(k, m)| {
                    let mut m = m.clone();
                    m.recv_ty = z(self.c, &m.recv_ty);
                    (*k, m)
                })
                .collect(),
            callees: self.callees.clone(),
            bounds: self.bounds.clone(),
        }
    }

    fn finish(&mut self) {
        let obligations = std::mem::take(&mut self.obligations);
        for (t, tr, span) in obligations {
            let t = self.c.infer.zonk(&t);
            if matches!(self.shallow(&t), Ty::Var(_)) {
                continue;
            }
            if !self.implements(&t, tr) {
                let (tn, trn) = (self.fmt(&t), self.c.r.def(tr).name.clone());
                self.error_help(
                    format!("the trait `{trn}` is not implemented for `{tn}`"),
                    span,
                    format!("add `impl {trn} for {tn}` or `#[derive({trn})]`"),
                );
            }
        }
        self.c.infer.apply_literal_defaults();
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
            return true;
        }
        let (e, f) = (self.fmt(expected), self.fmt(found));
        let mut d = kiln_lexer::Diagnostic::error(
            format!("mismatched types: expected `{e}`, found `{f}`"),
            span,
        );
        if let (Ty::Adt(sd, _), Ty::Ref(_, inner)) = (self.shallow(found), self.shallow(expected)) {
            if self.c.is_lang(sd, "String") && matches!(self.shallow(&inner), Ty::Prim(PrimTy::Str))
            {
                d = d.with_help("borrow it: `&value` (a `&String` coerces to `&str`)");
            }
        }
        self.c.errors.push(d);
        false
    }

    fn coerces(&mut self, found: &Ty, expected: &Ty) -> bool {
        let (f, e) = (self.shallow(found), self.shallow(expected));
        if matches!(f, Ty::Never) {
            return true;
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
                (Ty::Adt(d, _), Ty::Prim(PrimTy::Str)) if self.c.is_lang(*d, "String") => {
                    return true
                }
                (Ty::Adt(d, args), Ty::Slice(t)) if self.c.is_lang(*d, "Vec") => {
                    return self.c.infer.unify(&args[0], t)
                }
                (Ty::Array(a), Ty::Slice(t)) => return self.c.infer.unify(a, t),
                (Ty::Adt(d, args), _) if self.c.is_lang(*d, "Box") => {
                    return self.c.infer.try_unify(&args[0], &ei)
                }
                (_, Ty::Dyn(traits)) => {
                    let traits = traits.clone();
                    return traits.iter().all(|t| self.implements(&fi, *t));
                }
                _ => return false,
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
            Ty::Param(n) => self.param_bounds(n).contains(&tr),
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
            Ty::Array(x) | Ty::Slice(x) => {
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
            Ty::Fn(..) | Ty::ChanSend(_) | Ty::ChanRecv(_) => {
                marker(&["Copy", "Clone", "Send", "Sync"])
                    && !(marker(&["Copy"]) && matches!(t, Ty::ChanSend(_) | Ty::ChanRecv(_)))
            }
            Ty::Ptr(..) => marker(&["Copy", "Clone"]),
            Ty::Dyn(ts) => ts.iter().any(|d| self.supertrait_closure(*d).contains(&tr)),
            Ty::Adt(d, args) => {
                let d = *d;
                if self.c.r.def(d).is_prelude
                    && !self.c.adts[&d].is_enum
                    && self.c.r.def(d).name == "String"
                {
                    return marker(&[
                        "Clone", "Eq", "Ord", "Hash", "Debug", "Display", "Default", "Send", "Sync",
                    ]) || self.has_impl(&t, tr);
                }
                if self.c.r.def(d).is_prelude {
                    let args = args.clone();
                    let structural = marker(&[
                        "Clone", "Eq", "Ord", "Hash", "Debug", "Default", "Send", "Sync",
                    ]) || (tn == "Copy" && self.c.r.def(d).name == "Option");
                    return (structural && args.iter().all(|a| self.implements(a, tr)))
                        || self.has_impl(&t, tr);
                }
                if marker(&["Send", "Sync"]) {
                    // Auto traits: an explicit impl wins; otherwise all fields must implement it.
                    if self.has_impl(&t, tr) || self.auto_stack.contains(&d) {
                        return true;
                    }
                    let info = self.c.adts[&d].clone();
                    let map: HashMap<String, Ty> =
                        info.generics.iter().cloned().zip(args.iter().cloned()).collect();
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
            let (st, _) = self.instantiate_impl(idx);
            if self.c.infer.can_unify(&st, t) {
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
        Method {
            self_param: sig.self_param.clone(),
            params: sig.params.iter().map(|t| t.subst(&subst)).collect(),
            ret: sig.ret.subst(&subst),
            obligations,
            fn_idx: Some(i),
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
                let traits = self.param_bounds(n);
                return trait_method(self, traits, &t);
            }
            Ty::Dyn(traits) => return trait_method(self, traits.clone(), &t),
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
        None
    }

    /// One autoderef step.
    fn deref_step(&self, t: &Ty) -> Option<Ty> {
        match self.shallow(t) {
            Ty::Ref(_, x) | Ty::Ptr(_, x) => Some(*x),
            Ty::Adt(d, args) if self.c.is_lang(d, "Box") => Some(args[0].clone()),
            Ty::Adt(d, _) if self.c.is_lang(d, "String") => Some(Ty::Prim(PrimTy::Str)),
            Ty::Adt(d, args) if self.c.is_lang(d, "Vec") => {
                Some(Ty::Slice(Box::new(args[0].clone())))
            }
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
                let declared = ty.as_ref().map(|t| self.lower(t));
                let t = match (&declared, init) {
                    (Some(d), Some(e)) => {
                        let f = self.expr(e);
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
                            "cannot declare a variable from an expression of type `()`",
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
            StmtKind::Go(GoTarget::Call(e)) => {
                self.expr(e);
                Ty::Unit
            }
            StmtKind::Go(GoTarget::Block { body, .. }) => {
                self.rets.push(Ty::Unit);
                let saved = std::mem::take(&mut self.loops);
                self.block(body, false);
                self.loops = saved;
                self.rets.pop();
                Ty::Unit
            }
            StmtKind::Item(_) => Ty::Unit,
            StmtKind::Expr(e) => self.expr_v(e, value),
        }
    }

    /// Checks that `e` is a mutable place; `what` is e.g. "assign to".
    fn check_place_mut(&mut self, e: &Expr, what: &str) {
        match &e.kind {
            ExprKind::Path(p) => match self.c.r.path(p.span) {
                Res::Local(id) => {
                    let l = self.c.r.local(id).clone();
                    if !l.mutable {
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
                Res::Err | Res::External => {}
                _ => self.error(format!("cannot {what} this expression"), e.span),
            },
            ExprKind::Field(base, _) | ExprKind::TupleIndex(base, _) | ExprKind::Index(base, _) => {
                let bt = self.tys.get(&base.span).cloned().unwrap_or(Ty::Unknown);
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
                if let Ty::Ref(false, _) = self.shallow(&it) {
                    self.error_help(
                        format!("cannot {what} data behind a `&` reference"),
                        e.span,
                        "take a `&mut` reference instead",
                    );
                }
            }
            ExprKind::Paren(inner) => self.check_place_mut(inner, what),
            _ => self.error(format!("cannot {what} this expression"), e.span),
        }
    }

    // ----- expressions -----------------------------------------------------------

    /// Checks an expression in value position.
    fn expr(&mut self, e: &Expr) -> Ty {
        self.expr_v(e, true)
    }

    fn expr_v(&mut self, e: &Expr, value: bool) -> Ty {
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
            Lit::Char(_) => Ty::Prim(PrimTy::Char),
            Lit::Bool(_) => Ty::Prim(PrimTy::Bool),
        }
    }

    fn expr_inner(&mut self, e: &Expr, value: bool) -> Ty {
        match &e.kind {
            ExprKind::Lit(l) => self.lit_ty(l),
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.expr(x);
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
            ExprKind::Call(f, args) => self.call(f, args, e.span),
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
                Ty::Array(Box::new(elem))
            }
            ExprKind::ArrayRepeat(x, n) => {
                let t = self.expr(x);
                let nt = self.expr(n);
                self.coerce(&nt, &Ty::Prim(PrimTy::Usize), n.span);
                Ty::Array(Box::new(t))
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.block(b, value),
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
                let elem = self.iter_elem(&it, iter.span);
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
                        let t = self.expr(x);
                        if matches!(self.c.infer.zonk(&ret), Ty::Unit)
                            && !matches!(self.shallow(&t), Ty::Unit | Ty::Never)
                        {
                            let f = self.fmt(&t);
                            self.error_help(
                                format!("mismatched types: this function returns `()` but a value of type `{f}` was returned"),
                                x.span,
                                format!("add `-> {f}` to the function signature"),
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
                let pts: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.lower(t),
                        None => self.fresh(),
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
                Ty::Fn(pts, Box::new(rt))
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

    fn iter_elem(&mut self, t: &Ty, span: Span) -> Ty {
        match self.shallow(t) {
            Ty::Unknown => Ty::Unknown,
            Ty::Adt(d, args) if self.c.is_lang(d, "Range") || self.c.is_lang(d, "Vec") => {
                args[0].clone()
            }
            Ty::Array(x) | Ty::Slice(x) => *x,
            Ty::ChanRecv(x) => *x,
            Ty::Ref(m, inner) => match self.shallow(&inner) {
                Ty::Adt(d, args) if self.c.is_lang(d, "Vec") => {
                    Ty::Ref(m, Box::new(args[0].clone()))
                }
                Ty::Array(x) | Ty::Slice(x) => Ty::Ref(m, x),
                Ty::Unknown => Ty::Unknown,
                _ => self.not_iterable(t, span),
            },
            _ => self.not_iterable(t, span),
        }
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
            Shape::Tuple(ts) => Ty::Fn(ts.iter().map(|t| t.subst(&map)).collect(), Box::new(me)),
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
                        Some(ItemKind::Const { ty, .. }) => {
                            let ty = ty.clone();
                            self.c.lower_ty(&ty, None, false)
                        }
                        Some(ItemKind::Static { ty, .. }) => {
                            let ty = ty.clone();
                            self.c.lower_ty(&ty, None, false)
                        }
                        _ => Ty::Unknown,
                    },
                    DefKind::Variant { .. } => {
                        let (adt, shape) = self.variant_info(d);
                        let explicit = if p.segments.len() >= 2 {
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
                Ty::Fn(vec![v], Box::new(Ty::Unit))
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
    fn method_fn_ty(&mut self, m: Method, span: Span) -> Ty {
        if let Some(i) = m.fn_idx {
            self.callees.insert(span, i);
        }
        for (t, tr) in m.obligations {
            self.obligations.push((t, tr, span));
        }
        Ty::Fn(m.params, Box::new(m.ret))
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
            Some(m) => self.with_self_param(m, base_ty, span),
            None => {
                let t = self.fmt(&base_ty);
                self.error(
                    format!("no function or associated item named `{name}` found for `{t}`"),
                    span,
                );
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
        let def = self.c.r.def(d);
        def.is_prelude
            && matches!(
                def.name.as_str(),
                "print" | "println" | "eprint" | "eprintln"
            )
    }

    fn call(&mut self, f: &Expr, args: &[Expr], span: Span) -> Ty {
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
        let ft = self.expr(f);
        match self.shallow(&ft) {
            Ty::Fn(params, ret) => {
                self.check_args(&params, args, span);
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
                self.c.infer.unify(&ft, &Ty::Fn(pts, Box::new(ret.clone())));
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
            let t = self.expr(a);
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
                let self_param = Some(SelfParam::Ref { lifetime: None, mutable: false });
                let info = MethodInfo { steps, self_param, recv_ty: c.clone(), fn_idx: None };
                self.methods.insert(span, info);
                return t;
            }
            if let Some(m) = self.method_on(&c, name) {
                let info = MethodInfo {
                    steps,
                    self_param: m.self_param.clone(),
                    recv_ty: c.clone(),
                    fn_idx: m.fn_idx,
                };
                self.methods.insert(span, info);
                if let Some(SelfParam::Ref { mutable: true, .. }) = m.self_param {
                    if steps == 0 {
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
                self.check_args(&m.params, args, span);
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
                // The error type (`TrySendError<T>`) lives in std, which is opaque for now.
                return Some(self.c.result_ty(Ty::Unit, Ty::Unknown));
            }
            (Ty::ChanRecv(x), "try_recv") => self.c.result_ty((**x).clone(), Ty::Unknown),
            (Ty::ChanRecv(x), "recv") => self.c.option_ty((**x).clone()),
            (Ty::ChanSend(_), "close") => Ty::Unit,
            (Ty::ChanSend(_) | Ty::ChanRecv(_), "len" | "cap") => Ty::Prim(PrimTy::Usize),
            (Ty::ChanSend(_) | Ty::ChanRecv(_), "clone") => c.clone(),
            (
                Ty::Adt(..) | Ty::Prim(_) | Ty::Tuple(_) | Ty::Unit | Ty::Param(_) | Ty::Array(_),
                "clone",
            ) => {
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
                if let Some(d) = self.c.r.prelude_type("Display") {
                    self.obligations.push((c.clone(), d, span));
                }
                self.c.string_ty()
            }
            (Ty::Slice(x) | Ty::Array(x), "len") => {
                let _ = x;
                Ty::Prim(PrimTy::Usize)
            }
            (Ty::Slice(_) | Ty::Array(_), "is_empty") => Ty::Prim(PrimTy::Bool),
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
            match self.deref_step(&t) {
                Some(n)
                    if !matches!(self.shallow(&t), Ty::Adt(..))
                        || matches!(self.shallow(&t), Ty::Adt(d, _) if self.c.is_lang(d, "Box")) =>
                {
                    t = n
                }
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
        let t = self.strip_refs(bt);
        let elem = match &t {
            Ty::Adt(d, args) if self.c.is_lang(*d, "Vec") => args[0].clone(),
            Ty::Array(x) | Ty::Slice(x) => (**x).clone(),
            Ty::Adt(d, _) if self.c.is_lang(*d, "String") => {
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
            Ty::Adt(d, args) if self.c.is_lang(d, "Result") => {
                match &ret {
                    Ty::Adt(rd, rargs) if self.c.is_lang(*rd, "Result") => {
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
                    other => self.try_outside(other, "Result", span),
                }
                args[0].clone()
            }
            Ty::Adt(d, args) if self.c.is_lang(d, "Option") => {
                match &ret {
                    Ty::Adt(rd, _) if self.c.is_lang(*rd, "Option") => {}
                    Ty::Unknown | Ty::Var(_) => {}
                    other => self.try_outside(other, "Option", span),
                }
                args[0].clone()
            }
            other => {
                let tn = self.fmt(&other);
                self.error(format!("the `?` operator can only be applied to `Result` or `Option`, found `{tn}`"), span);
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
                if self.c.is_lang(*d, "String") {
                    if !matches!(r, Ty::Prim(PrimTy::Str))
                        && !matches!(&r, Ty::Adt(d2, _) if self.c.is_lang(*d2, "String"))
                    {
                        let f = self.fmt(rt);
                        self.error(format!("cannot add `{f}` to `String`"), span);
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
                        || matches!(t, Ty::Adt(d, _) if s.c.is_lang(*d, "String"))
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
                Ty::Ref(_, i) | Ty::Ptr(_, i) => *i,
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
                    // A unit variant / unit struct / const used as a pattern.
                    self.path_pat(name.span, &t, p.span);
                }
                if let Some(s) = sub {
                    self.bind_pat(s, &t, mode);
                }
            }
            PatternKind::Lit(l) => {
                let lt = self.lit_ty(l);
                let lt = match (&lt, &t) {
                    (Ty::Ref(_, x), Ty::Prim(PrimTy::Str)) => (**x).clone(),
                    (Ty::Ref(_, _), Ty::Adt(d, _)) if self.c.is_lang(*d, "String") => t.clone(),
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
                    Ty::Array(x) | Ty::Slice(x) => (**x).clone(),
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

    /// Whether `p` matches every value of its type (ignoring types it can't see).
    fn irrefutable(&self, p: &Pattern) -> bool {
        match &p.kind {
            PatternKind::Wild | PatternKind::Rest => true,
            PatternKind::Ident { name, sub, .. } => {
                self.c.r.bindings.contains_key(&name.span)
                    && sub.as_ref().is_none_or(|s| self.irrefutable(s))
            }
            PatternKind::Tuple(ps) => ps.iter().all(|x| self.irrefutable(x)),
            PatternKind::Ref { inner, .. } => self.irrefutable(inner),
            PatternKind::Or(ps) => ps.iter().any(|x| self.irrefutable(x)),
            PatternKind::Struct { path, fields, .. } => {
                matches!(self.c.r.path(path.span), Res::Def(d) if self.c.r.def(d).kind == DefKind::Struct)
                    && fields.iter().all(|f| self.irrefutable(&f.pat))
            }
            PatternKind::TupleStruct(path, ps) => {
                matches!(self.c.r.path(path.span), Res::Def(d) if self.c.r.def(d).kind == DefKind::Struct)
                    && ps.iter().all(|x| self.irrefutable(x))
            }
            _ => false,
        }
    }

    /// Variant names fully covered by `p` (top-level variant with irrefutable fields).
    fn covered_variants(&self, p: &Pattern, out: &mut Vec<String>) {
        let variant_name = |path: &Path| -> Option<String> {
            match self.c.r.path(path.span) {
                Res::Def(d) if matches!(self.c.r.def(d).kind, DefKind::Variant { .. }) => {
                    Some(self.c.r.def(d).name.clone())
                }
                Res::Assoc { name, .. } => Some(name),
                _ => None,
            }
        };
        match &p.kind {
            PatternKind::Or(ps) => ps.iter().for_each(|x| self.covered_variants(x, out)),
            PatternKind::Ref { inner, .. } => self.covered_variants(inner, out),
            PatternKind::Path(path) => out.extend(variant_name(path)),
            PatternKind::Ident { name, .. } if !self.c.r.bindings.contains_key(&name.span) => {
                if let Res::Def(d) = self.c.r.path(name.span) {
                    out.push(self.c.r.def(d).name.clone());
                }
            }
            PatternKind::TupleStruct(path, ps) if ps.iter().all(|x| self.irrefutable(x)) => {
                out.extend(variant_name(path))
            }
            PatternKind::Struct { path, fields, .. }
                if fields.iter().all(|f| self.irrefutable(&f.pat)) =>
            {
                out.extend(variant_name(path))
            }
            PatternKind::Lit(Lit::Bool(b)) => out.push(b.to_string()),
            _ => {}
        }
    }

    fn check_exhaustive(&mut self, st: &Ty, arms: &[MatchArm], span: Span) {
        let unguarded: Vec<&MatchArm> = arms.iter().filter(|a| a.guard.is_none()).collect();
        if unguarded.iter().any(|a| self.irrefutable(&a.pat)) {
            return;
        }
        let mut covered = Vec::new();
        for a in &unguarded {
            self.covered_variants(&a.pat, &mut covered);
        }
        let t = self.strip_refs(st);
        let all: Vec<String> = match &t {
            Ty::Adt(d, _) if self.c.adts[d].is_enum => self.c.adts[d]
                .variants
                .iter()
                .map(|(v, _)| self.c.r.def(*v).name.clone())
                .collect(),
            Ty::Prim(PrimTy::Bool) => vec!["true".into(), "false".into()],
            Ty::Unknown | Ty::Never => return,
            _ => {
                let f = self.fmt(&t);
                self.error_help(
                    format!("non-exhaustive match: not all values of `{f}` are covered"),
                    span,
                    "add a `_ => ...` arm",
                );
                return;
            }
        };
        let missing: Vec<String> = all
            .into_iter()
            .filter(|v| !covered.contains(v))
            .map(|v| format!("`{v}`"))
            .collect();
        if !missing.is_empty() {
            self.error_help(
                format!("non-exhaustive match: {} not covered", missing.join(", ")),
                span,
                "add arms for the missing cases or a `_ => ...` arm",
            );
        }
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
