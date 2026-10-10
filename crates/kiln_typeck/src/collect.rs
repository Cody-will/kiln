//! Item collection: lowers struct/enum/fn/impl/trait signatures to [`Ty`] and
//! validates trait implementations.

use crate::ty::{InferTable, Ty, VarKind};
use kiln_lexer::{Diagnostic, Span};
use kiln_parser::ast::{
    self, Bound, Fields, GenericArg, GenericParam, ItemKind, SelfParam, TypeKind,
};
use kiln_resolve::{DefId, DefKind, PrimTy, Res, Resolved};
use std::collections::HashMap;

/// Field layout of a struct or enum variant.
#[derive(Debug, Clone)]
pub(crate) enum Shape {
    Unit,
    Tuple(Vec<Ty>),
    Named(Vec<(String, Ty)>),
}

/// A lowered struct or enum.
#[derive(Debug, Clone)]
pub(crate) struct AdtInfo {
    pub generics: Vec<String>,
    /// For structs: the fields. For enums: unused (`Unit`).
    pub shape: Shape,
    /// For enums: `(variant def, shape)` in declaration order.
    pub variants: Vec<(DefId, Shape)>,
    /// Trait names listed in `#[derive(...)]`.
    pub derives: Vec<String>,
    pub is_enum: bool,
}

/// A lowered function signature (free function, method, or trait method).
#[derive(Debug, Clone)]
pub(crate) struct FnSig {
    /// Generic parameters of the function itself.
    pub generics: Vec<String>,
    /// Generic parameters of the enclosing impl or trait.
    pub outer_generics: Vec<String>,
    /// `(param name, trait)` bounds in scope, including the enclosing impl's.
    pub bounds: Vec<(String, DefId)>,
    pub self_param: Option<SelfParam>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub has_body: bool,
}

/// A lowered `impl` block.
#[derive(Debug, Clone)]
pub(crate) struct ImplInfo {
    pub generics: Vec<String>,
    pub self_ty: Ty,
    pub trait_ref: Option<(DefId, Vec<Ty>)>,
    /// Bounds on the impl's generic parameters (`impl<T: Send> ...`).
    pub bounds: Vec<(String, DefId)>,
    /// Method name → index into [`Checker::sigs`] / `Resolved::fns`.
    pub methods: HashMap<String, usize>,
}

/// A lowered trait.
#[derive(Debug, Clone, Default)]
pub(crate) struct TraitInfo {
    pub generics: Vec<String>,
    pub supertraits: Vec<DefId>,
    pub methods: HashMap<String, usize>,
}

/// Shared type-checking state.
pub(crate) struct Checker<'r, 'a> {
    pub r: &'r Resolved<'a>,
    pub adts: HashMap<DefId, AdtInfo>,
    pub sigs: Vec<FnSig>,
    pub impls: Vec<ImplInfo>,
    pub traits: HashMap<DefId, TraitInfo>,
    pub infer: InferTable,
    pub errors: Vec<Diagnostic>,
    /// Per-function type results (indexed like `Resolved::fns`).
    pub results: Vec<Option<crate::check::FnResults>>,
}

/// Names of the generic type parameters declared in `g`.
pub(crate) fn generic_names(g: &ast::Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect()
}

impl<'r, 'a> Checker<'r, 'a> {
    pub(crate) fn new(r: &'r Resolved<'a>) -> Self {
        Checker {
            r,
            adts: HashMap::new(),
            sigs: Vec::new(),
            impls: Vec::new(),
            traits: HashMap::new(),
            infer: InferTable::default(),
            errors: Vec::new(),
            results: Vec::new(),
        }
    }

    pub(crate) fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::error(msg, span));
    }

    pub(crate) fn error_help(
        &mut self,
        msg: impl Into<String>,
        span: Span,
        help: impl Into<String>,
    ) {
        self.errors
            .push(Diagnostic::error(msg, span).with_help(help));
    }

    /// A prelude type such as `Option` (panics if missing: compiler bug).
    pub(crate) fn lang(&self, name: &str) -> DefId {
        self.r
            .prelude_type(name)
            .unwrap_or_else(|| panic!("prelude type `{name}` missing"))
    }

    pub(crate) fn string_ty(&self) -> Ty {
        Ty::Adt(self.lang("String"), vec![])
    }

    pub(crate) fn option_ty(&self, t: Ty) -> Ty {
        Ty::Adt(self.lang("Option"), vec![t])
    }

    pub(crate) fn result_ty(&self, t: Ty, e: Ty) -> Ty {
        Ty::Adt(self.lang("Result"), vec![t, e])
    }

    pub(crate) fn is_lang(&self, d: DefId, name: &str) -> bool {
        self.r.prelude_type(name) == Some(d)
    }

    // ----- type lowering -------------------------------------------------------

    /// Lowers a syntactic type. `self_ty` is what `Self` means; `infer` allows
    /// `_` and omitted generic arguments (inside function bodies).
    pub(crate) fn lower_ty(&mut self, t: &ast::Type, self_ty: Option<&Ty>, infer: bool) -> Ty {
        match &t.kind {
            TypeKind::Path(p) => self.lower_path_ty(p, self_ty, infer),
            TypeKind::Ref { mutable, inner, .. } => {
                Ty::Ref(*mutable, Box::new(self.lower_ty(inner, self_ty, infer)))
            }
            TypeKind::Ptr { mutable, inner } => {
                Ty::Ptr(*mutable, Box::new(self.lower_ty(inner, self_ty, infer)))
            }
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.lower_ty(inner, self_ty, infer))),
            TypeKind::Array(inner, len) => {
                let n = array_len(len).unwrap_or_else(|| {
                    self.error("array lengths must be integer literals for now", len.span);
                    0
                });
                Ty::Array(Box::new(self.lower_ty(inner, self_ty, infer)), n)
            }
            TypeKind::Tuple(ts) if ts.is_empty() => Ty::Unit,
            TypeKind::Tuple(ts) => Ty::Tuple(
                ts.iter()
                    .map(|x| self.lower_ty(x, self_ty, infer))
                    .collect(),
            ),
            TypeKind::Fn { params, ret } => Ty::Fn(
                params
                    .iter()
                    .map(|x| self.lower_ty(x, self_ty, infer))
                    .collect(),
                Box::new(
                    ret.as_ref()
                        .map(|r| self.lower_ty(r, self_ty, infer))
                        .unwrap_or(Ty::Unit),
                ),
            ),
            TypeKind::Dyn(bounds) | TypeKind::Impl(bounds) => Ty::Dyn(self.bound_traits(bounds)),
            TypeKind::ChanSend(x) => Ty::ChanSend(Box::new(self.lower_ty(x, self_ty, infer))),
            TypeKind::ChanRecv(x) => Ty::ChanRecv(Box::new(self.lower_ty(x, self_ty, infer))),
            TypeKind::Never => Ty::Never,
            TypeKind::Infer if infer => self.infer.fresh(VarKind::General),
            TypeKind::Infer => {
                self.error(
                    "the placeholder `_` is not allowed in item signatures",
                    t.span,
                );
                Ty::Unknown
            }
        }
    }

    pub(crate) fn bound_traits(&self, bounds: &[Bound]) -> Vec<DefId> {
        bounds
            .iter()
            .filter_map(|b| match b {
                Bound::Trait { path, maybe: false } => match self.r.path(path.span) {
                    Res::Def(d) if self.r.def(d).kind == DefKind::Trait => Some(d),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn lower_args(&mut self, args: &[GenericArg], self_ty: Option<&Ty>, infer: bool) -> Vec<Ty> {
        args.iter()
            .filter_map(|a| match a {
                GenericArg::Type(t) => Some(self.lower_ty(t, self_ty, infer)),
                _ => None,
            })
            .collect()
    }

    fn lower_path_ty(&mut self, p: &ast::Path, self_ty: Option<&Ty>, infer: bool) -> Ty {
        let args = self.lower_args(&p.segments.last().unwrap().args, self_ty, infer);
        match self.r.path(p.span) {
            Res::Prim(PrimTy::Str) => Ty::Prim(PrimTy::Str),
            Res::Prim(prim) => Ty::Prim(prim),
            Res::TyParam(n) => Ty::Param(n),
            Res::SelfTy => self_ty.cloned().unwrap_or_else(|| Ty::Param("Self".into())),
            Res::Def(d) => match self.r.def(d).kind {
                DefKind::Struct | DefKind::Enum => {
                    let expected = self.adt_generic_count(d);
                    let args = self.fit_args(d, args, expected, p.span, infer);
                    Ty::Adt(d, args)
                }
                DefKind::TypeAlias => self.lower_alias(d, args, p.span),
                DefKind::Trait => {
                    let name = self.r.def(d).name.clone();
                    self.error_help(
                        format!("trait `{name}` used as a type"),
                        p.span,
                        format!("use `dyn {name}` for a trait object or a generic `T: {name}`"),
                    );
                    Ty::Unknown
                }
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    fn adt_generic_count(&self, d: DefId) -> usize {
        match self.r.def(d).item.map(|i| &i.kind) {
            Some(ItemKind::Struct(s)) => generic_names(&s.generics).len(),
            Some(ItemKind::Enum(e)) => generic_names(&e.generics).len(),
            _ => 0,
        }
    }

    fn fit_args(
        &mut self,
        d: DefId,
        args: Vec<Ty>,
        expected: usize,
        span: Span,
        infer: bool,
    ) -> Vec<Ty> {
        if args.len() == expected {
            return args;
        }
        if args.is_empty() && infer {
            return (0..expected)
                .map(|_| self.infer.fresh(VarKind::General))
                .collect();
        }
        let name = self.r.def(d).name.clone();
        self.error(
            format!(
                "`{name}` takes {expected} generic argument{} but {} {} supplied",
                if expected == 1 { "" } else { "s" },
                args.len(),
                if args.len() == 1 { "was" } else { "were" }
            ),
            span,
        );
        (0..expected).map(|_| Ty::Unknown).collect()
    }

    fn lower_alias(&mut self, d: DefId, args: Vec<Ty>, span: Span) -> Ty {
        let Some(ItemKind::TypeAlias {
            generics,
            ty: Some(t),
            ..
        }) = self.r.def(d).item.map(|i| &i.kind)
        else {
            return Ty::Unknown;
        };
        let names = generic_names(generics);
        let lowered = self.lower_ty(t, None, false);
        if names.len() != args.len() {
            let name = self.r.def(d).name.clone();
            self.error(
                format!(
                    "type alias `{name}` takes {} generic arguments but {} were supplied",
                    names.len(),
                    args.len()
                ),
                span,
            );
            return Ty::Unknown;
        }
        lowered.subst(&names.into_iter().zip(args).collect())
    }

    // ----- collection ------------------------------------------------------------

    pub(crate) fn collect(&mut self) {
        // ADTs.
        for (i, def) in self.r.defs.iter().enumerate() {
            let id = DefId(i as u32);
            let Some(item) = def.item else { continue };
            match &item.kind {
                ItemKind::Struct(s) => {
                    let shape = self.lower_fields(&s.fields);
                    let info = AdtInfo {
                        generics: generic_names(&s.generics),
                        shape,
                        variants: vec![],
                        derives: derives(item),
                        is_enum: false,
                    };
                    self.adts.insert(id, info);
                }
                ItemKind::Enum(e) => {
                    let variants = self
                        .r
                        .variants(id)
                        .into_iter()
                        .map(|v| {
                            let fields = &self.r.def(v).variant.unwrap().fields;
                            (v, self.lower_fields(fields))
                        })
                        .collect();
                    let info = AdtInfo {
                        generics: generic_names(&e.generics),
                        shape: Shape::Unit,
                        variants,
                        derives: derives(item),
                        is_enum: true,
                    };
                    self.adts.insert(id, info);
                }
                _ => {}
            }
        }
        // Traits.
        for (i, def) in self.r.defs.iter().enumerate() {
            if let Some(ItemKind::Trait(t)) = def.item.map(|i| &i.kind) {
                let info = TraitInfo {
                    generics: generic_names(&t.generics),
                    supertraits: self.bound_traits(&t.supertraits),
                    methods: HashMap::new(),
                };
                self.traits.insert(DefId(i as u32), info);
            }
        }
        // Impl headers.
        for imp in &self.r.impls {
            let generics = generic_names(&imp.decl.generics);
            let self_ty = self.lower_ty(&imp.decl.self_ty, None, false);
            let trait_ref = imp
                .decl
                .trait_ref
                .as_ref()
                .and_then(|p| match self.r.path(p.span) {
                    Res::Def(d) if self.r.def(d).kind == DefKind::Trait => {
                        let args = self.lower_args(
                            &p.segments.last().unwrap().args,
                            Some(&self_ty),
                            false,
                        );
                        Some((d, args))
                    }
                    _ => None,
                });
            if trait_ref.is_none() && imp.decl.trait_ref.is_none() && !imp.is_prelude {
                let local = matches!(&self_ty, Ty::Adt(d, _) if !self.r.def(*d).is_prelude)
                    || self_ty == Ty::Unknown;
                if !local {
                    let t = self.fmt_ty(&self_ty);
                    self.error_help(
                        format!("cannot define inherent `impl` for type `{t}` defined outside this package"),
                        imp.decl.self_ty.span,
                        "define a trait and implement it for the type instead",
                    );
                }
            }
            if let (Some((tr, _)), Some(path)) = (&trait_ref, &imp.decl.trait_ref) {
                let unsafe_trait = matches!(
                    self.r.def(*tr).item.map(|i| &i.kind),
                    Some(kiln_parser::ast::ItemKind::Trait(t)) if t.is_unsafe
                );
                let name = self.r.def(*tr).name.clone();
                if unsafe_trait && !imp.decl.is_unsafe {
                    self.error_help(
                        format!("the trait `{name}` requires an `unsafe impl` declaration"),
                        path.span,
                        format!("write `unsafe impl {name} for ...`: implementing `{name}` manually promises thread safety the compiler can't check"),
                    );
                } else if !unsafe_trait && imp.decl.is_unsafe {
                    self.error_help(
                        format!("implementing the trait `{name}` is not unsafe"),
                        path.span,
                        "remove `unsafe` from this impl",
                    );
                }
            } else if imp.decl.is_unsafe && imp.decl.trait_ref.is_none() {
                self.error_help(
                    "inherent impls cannot be unsafe",
                    imp.decl.self_ty.span,
                    "remove `unsafe`",
                );
            }
            let mut bounds = Vec::new();
            self.push_bounds(&imp.decl.generics, None, &mut bounds);
            self.impls.push(ImplInfo {
                generics,
                self_ty,
                trait_ref,
                bounds,
                methods: HashMap::new(),
            });
        }
        // Function signatures.
        for i in 0..self.r.fns.len() {
            let sig = self.lower_sig(i);
            self.sigs.push(sig);
            let f = &self.r.fns[i];
            let name = f.decl.name.name.clone();
            if let Some(idx) = f.impl_idx {
                if self.impls[idx].methods.insert(name.clone(), i).is_some() {
                    self.error(
                        format!("duplicate definitions with name `{name}`"),
                        f.decl.name.span,
                    );
                }
            } else if let Some(t) = f.trait_def {
                self.traits.get_mut(&t).unwrap().methods.insert(name, i);
            }
        }
        self.results = vec![None; self.r.fns.len()];
        // Duplicate inherent methods across impl blocks of the same type.
        let mut seen: HashMap<(String, String), Span> = HashMap::new();
        for idx in 0..self.impls.len() {
            if self.impls[idx].trait_ref.is_some() {
                continue;
            }
            let ty = self.fmt_ty(&self.impls[idx].self_ty);
            let mut names: Vec<_> = self.impls[idx]
                .methods
                .iter()
                .map(|(n, i)| (n.clone(), *i))
                .collect();
            names.sort();
            for (n, i) in names {
                let span = self.r.fns[i].decl.name.span;
                if seen.insert((ty.clone(), n.clone()), span).is_some() {
                    self.error(
                        format!("duplicate definitions with name `{n}` for type `{ty}`"),
                        span,
                    );
                }
            }
        }
        for idx in 0..self.impls.len() {
            self.check_trait_impl(idx);
        }
    }

    fn lower_fields(&mut self, f: &Fields) -> Shape {
        match f {
            Fields::Unit => Shape::Unit,
            Fields::Tuple(ts) => {
                Shape::Tuple(ts.iter().map(|t| self.lower_ty(t, None, false)).collect())
            }
            Fields::Named(fs) => Shape::Named(
                fs.iter()
                    .map(|f| (f.name.name.clone(), self.lower_ty(&f.ty, None, false)))
                    .collect(),
            ),
        }
    }

    fn lower_sig(&mut self, i: usize) -> FnSig {
        let f = self.r.fns[i].clone();
        let mut bounds = Vec::new();
        let mut outer_generics = Vec::new();
        let self_ty = if let Some(idx) = f.impl_idx {
            let decl = self.r.impls[idx].decl;
            outer_generics = generic_names(&decl.generics);
            self.push_bounds(&decl.generics, None, &mut bounds);
            Some(self.impls[idx].self_ty.clone())
        } else if let Some(t) = f.trait_def {
            bounds.push(("Self".to_string(), t));
            if let Some(ItemKind::Trait(td)) = self.r.def(t).item.map(|i| &i.kind) {
                outer_generics = generic_names(&td.generics);
            }
            None
        } else {
            None
        };
        let sty = self_ty.as_ref();
        self.push_bounds(&f.decl.generics, sty, &mut bounds);
        let params = f
            .decl
            .params
            .iter()
            .map(|p| self.lower_ty(&p.ty, sty, false))
            .collect();
        let ret = f
            .decl
            .ret
            .as_ref()
            .map(|t| self.lower_ty(t, sty, false))
            .unwrap_or(Ty::Unit);
        FnSig {
            generics: generic_names(&f.decl.generics),
            outer_generics,
            bounds,
            self_param: f.decl.self_param.clone(),
            params,
            ret,
            has_body: f.decl.body.is_some(),
        }
    }

    fn push_bounds(
        &mut self,
        g: &ast::Generics,
        self_ty: Option<&Ty>,
        out: &mut Vec<(String, DefId)>,
    ) {
        for p in &g.params {
            if let GenericParam::Type { name, bounds, .. } = p {
                for t in self.bound_traits(bounds) {
                    out.push((name.name.clone(), t));
                }
            }
        }
        for w in g.where_clause.iter().filter(|w| w.lifetime.is_none()) {
            match self.lower_ty(&w.ty, self_ty, false) {
                Ty::Param(n) => {
                    for t in self.bound_traits(&w.bounds) {
                        out.push((n.clone(), t));
                    }
                }
                _ => self.error(
                    "`where` clauses currently only support generic parameters (`T: Trait`)",
                    w.span,
                ),
            }
        }
    }

    /// Checks that a trait impl provides exactly the trait's methods with matching signatures.
    fn check_trait_impl(&mut self, idx: usize) {
        let Some((tr, trait_args)) = self.impls[idx].trait_ref.clone() else {
            return;
        };
        let span = self.r.impls[idx].span;
        let tinfo = self.traits.get(&tr).cloned().unwrap_or_default();
        let tname = self.r.def(tr).name.clone();
        let self_ty = self.impls[idx].self_ty.clone();
        let mut subst: HashMap<String, Ty> = tinfo
            .generics
            .iter()
            .cloned()
            .zip(trait_args.iter().cloned())
            .collect();
        subst.insert("Self".into(), self_ty.clone());
        let mut missing: Vec<String> = tinfo
            .methods
            .iter()
            .filter(|(n, i)| !self.sigs[**i].has_body && !self.impls[idx].methods.contains_key(*n))
            .map(|(n, _)| n.clone())
            .collect();
        missing.sort();
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.error_help(
                format!("not all trait items implemented, missing: {list}"),
                span,
                format!("implement the missing method(s) of trait `{tname}`"),
            );
        }
        let mut methods: Vec<_> = self.impls[idx].methods.clone().into_iter().collect();
        methods.sort();
        for (name, fi) in methods {
            let name_span = self.r.fns[fi].decl.name.span;
            let Some(&ti) = tinfo.methods.get(&name) else {
                self.error(
                    format!("method `{name}` is not a member of trait `{tname}`"),
                    name_span,
                );
                continue;
            };
            let (want, have) = (self.sigs[ti].clone(), self.sigs[fi].clone());
            let want_params: Vec<Ty> = want.params.iter().map(|t| t.subst(&subst)).collect();
            let want_ret = want.ret.subst(&subst);
            let same_self = match (&want.self_param, &have.self_param) {
                (None, None) => true,
                (
                    Some(SelfParam::Ref { mutable: a, .. }),
                    Some(SelfParam::Ref { mutable: b, .. }),
                ) => a == b,
                (Some(SelfParam::Value { .. }), Some(SelfParam::Value { .. })) => true,
                _ => false,
            };
            let same = same_self && want_params == have.params && want_ret == have.ret;
            if !same {
                let w = self.fmt_sig(&want.self_param, &want_params, &want_ret);
                let h = self.fmt_sig(&have.self_param, &have.params, &have.ret);
                self.error_help(
                    format!("method `{name}` has an incompatible signature for trait `{tname}`"),
                    name_span,
                    format!("expected `{w}`, found `{h}`"),
                );
            }
        }
    }

    pub(crate) fn fmt_sig(&self, sp: &Option<SelfParam>, params: &[Ty], ret: &Ty) -> String {
        let mut ps: Vec<String> = Vec::new();
        match sp {
            Some(SelfParam::Value { .. }) => ps.push("self".into()),
            Some(SelfParam::Ref { mutable: false, .. }) => ps.push("&self".into()),
            Some(SelfParam::Ref { mutable: true, .. }) => ps.push("&mut self".into()),
            None => {}
        }
        ps.extend(params.iter().map(|t| self.fmt_ty(t)));
        match ret {
            Ty::Unit => format!("fn({})", ps.join(", ")),
            r => format!("fn({}) -> {}", ps.join(", "), self.fmt_ty(r)),
        }
    }
}

/// Trait names from `#[derive(A, B)]` attributes.
fn derives(item: &ast::Item) -> Vec<String> {
    item.attrs
        .iter()
        .filter(|a| a.name.name == "derive")
        .flat_map(|a| {
            a.args
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The length in `[T; N]` or `[x; N]`: an integer literal (constants are not evaluated yet).
pub(crate) fn array_len(e: &kiln_parser::ast::Expr) -> Option<u64> {
    match &e.kind {
        kiln_parser::ast::ExprKind::Lit(kiln_parser::ast::Lit::Int {
            value,
            negative: false,
            ..
        }) => u64::try_from(*value).ok(),
        kiln_parser::ast::ExprKind::Paren(x) => array_len(x),
        _ => None,
    }
}
