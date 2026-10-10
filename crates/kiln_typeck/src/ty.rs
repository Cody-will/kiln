//! Types and the unification table.

use kiln_resolve::{DefId, PrimTy};
use std::collections::HashMap;

/// A Kiln type during and after inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// A primitive (`i32`, `f64`, `bool`, `char`, `str`, ...).
    Prim(PrimTy),
    /// `()`
    Unit,
    /// `!` (the type of `return`, `break`, `panic(...)`).
    Never,
    /// `(A, B, ...)` with at least one element.
    Tuple(Vec<Ty>),
    /// `&T` / `&mut T`
    Ref(bool, Box<Ty>),
    /// `*const T` / `*mut T`
    Ptr(bool, Box<Ty>),
    /// `[T; N]` (the length is not tracked yet).
    Array(Box<Ty>),
    /// `[T]`
    Slice(Box<Ty>),
    /// A struct or enum with generic arguments.
    Adt(DefId, Vec<Ty>),
    /// A function or closure type.
    Fn(Vec<Ty>, Box<Ty>),
    /// A generic parameter in scope (by name; `Self` inside traits).
    Param(String),
    /// `dyn Trait + ...`
    Dyn(Vec<DefId>),
    /// `chan<- T`
    ChanSend(Box<Ty>),
    /// `<-chan T`
    ChanRecv(Box<Ty>),
    /// An inference variable.
    Var(u32),
    /// An opaque or erroneous type that unifies with anything (used for
    /// external items and to avoid cascading errors).
    Unknown,
}

impl Ty {
    /// `&T`
    pub fn r#ref(t: Ty) -> Ty {
        Ty::Ref(false, Box::new(t))
    }

    /// Whether this is an integer primitive.
    pub fn is_int_prim(&self) -> bool {
        matches!(
            self,
            Ty::Prim(
                PrimTy::I8
                    | PrimTy::I16
                    | PrimTy::I32
                    | PrimTy::I64
                    | PrimTy::I128
                    | PrimTy::Isize
                    | PrimTy::U8
                    | PrimTy::U16
                    | PrimTy::U32
                    | PrimTy::U64
                    | PrimTy::U128
                    | PrimTy::Usize
            )
        )
    }

    /// Whether this is a float primitive.
    pub fn is_float_prim(&self) -> bool {
        matches!(self, Ty::Prim(PrimTy::F32 | PrimTy::F64))
    }

    /// Replaces generic parameters according to `map`.
    pub fn subst(&self, map: &HashMap<String, Ty>) -> Ty {
        if map.is_empty() {
            return self.clone();
        }
        let s = |t: &Ty| Box::new(t.subst(map));
        match self {
            Ty::Param(n) => map.get(n).cloned().unwrap_or_else(|| self.clone()),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| t.subst(map)).collect()),
            Ty::Ref(m, t) => Ty::Ref(*m, s(t)),
            Ty::Ptr(m, t) => Ty::Ptr(*m, s(t)),
            Ty::Array(t) => Ty::Array(s(t)),
            Ty::Slice(t) => Ty::Slice(s(t)),
            Ty::Adt(d, args) => Ty::Adt(*d, args.iter().map(|t| t.subst(map)).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|t| t.subst(map)).collect(), s(r)),
            Ty::ChanSend(t) => Ty::ChanSend(s(t)),
            Ty::ChanRecv(t) => Ty::ChanRecv(s(t)),
            _ => self.clone(),
        }
    }
}

/// What an inference variable may be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    /// Any type.
    General,
    /// An integer type (from an unsuffixed integer literal).
    Int,
    /// A float type (from an unsuffixed float literal).
    Float,
}

/// Union-find style table of inference variables.
#[derive(Debug, Default, Clone)]
pub struct InferTable {
    vars: Vec<(Option<Ty>, VarKind)>,
}

impl InferTable {
    /// Creates a fresh variable.
    pub fn fresh(&mut self, kind: VarKind) -> Ty {
        self.vars.push((None, kind));
        Ty::Var(self.vars.len() as u32 - 1)
    }

    /// Kind of an unbound variable.
    pub fn kind(&self, v: u32) -> VarKind {
        self.vars[v as usize].1
    }

    /// Follows variable bindings at the top level.
    pub fn shallow(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(v) = t {
            match &self.vars[v as usize].0 {
                Some(b) => t = b.clone(),
                None => return t,
            }
        }
        t
    }

    /// Fully substitutes bound variables.
    pub fn zonk(&self, t: &Ty) -> Ty {
        let t = self.shallow(t);
        let z = |x: &Ty| Box::new(self.zonk(x));
        match &t {
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.zonk(x)).collect()),
            Ty::Ref(m, x) => Ty::Ref(*m, z(x)),
            Ty::Ptr(m, x) => Ty::Ptr(*m, z(x)),
            Ty::Array(x) => Ty::Array(z(x)),
            Ty::Slice(x) => Ty::Slice(z(x)),
            Ty::Adt(d, args) => Ty::Adt(*d, args.iter().map(|x| self.zonk(x)).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(|x| self.zonk(x)).collect(), z(r)),
            Ty::ChanSend(x) => Ty::ChanSend(z(x)),
            Ty::ChanRecv(x) => Ty::ChanRecv(z(x)),
            _ => t,
        }
    }

    fn occurs(&self, v: u32, t: &Ty) -> bool {
        match self.shallow(t) {
            Ty::Var(w) => w == v,
            Ty::Tuple(ts) => ts.iter().any(|x| self.occurs(v, x)),
            Ty::Adt(_, ts) => ts.iter().any(|x| self.occurs(v, x)),
            Ty::Fn(ps, r) => ps.iter().any(|x| self.occurs(v, x)) || self.occurs(v, &r),
            Ty::Ref(_, x)
            | Ty::Ptr(_, x)
            | Ty::Array(x)
            | Ty::Slice(x)
            | Ty::ChanSend(x)
            | Ty::ChanRecv(x) => self.occurs(v, &x),
            _ => false,
        }
    }

    fn bind(&mut self, v: u32, t: Ty) -> bool {
        let ok = match self.kind(v) {
            VarKind::General => true,
            VarKind::Int => match &t {
                Ty::Var(w) => self.kind(*w) != VarKind::Float,
                other => other.is_int_prim(),
            },
            VarKind::Float => match &t {
                Ty::Var(w) => self.kind(*w) != VarKind::Int,
                other => other.is_float_prim(),
            },
        };
        if !ok || self.occurs(v, &t) {
            return false;
        }
        // Keep the more specific kind when binding var to var.
        if let Ty::Var(w) = t {
            if self.kind(w) == VarKind::General && self.kind(v) != VarKind::General {
                let k = self.kind(v);
                self.vars[w as usize].1 = k;
            }
        }
        self.vars[v as usize].0 = Some(t);
        true
    }

    /// Unifies two types, binding variables. Returns false on mismatch (bindings
    /// made before the mismatch are kept; callers report an error).
    pub fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let (a, b) = (self.shallow(a), self.shallow(b));
        match (&a, &b) {
            (Ty::Unknown, _) | (_, Ty::Unknown) => true,
            (Ty::Var(x), Ty::Var(y)) if x == y => true,
            (Ty::Var(v), t) | (t, Ty::Var(v)) => self.bind(*v, t.clone()),
            (Ty::Never, Ty::Never) => true,
            (Ty::Prim(x), Ty::Prim(y)) => x == y,
            (Ty::Unit, Ty::Unit) => true,
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Ref(m1, x), Ty::Ref(m2, y)) | (Ty::Ptr(m1, x), Ty::Ptr(m2, y)) => {
                m1 == m2 && self.unify(x, y)
            }
            (Ty::Array(x), Ty::Array(y))
            | (Ty::Slice(x), Ty::Slice(y))
            | (Ty::ChanSend(x), Ty::ChanSend(y))
            | (Ty::ChanRecv(x), Ty::ChanRecv(y)) => self.unify(x, y),
            (Ty::Adt(d1, xs), Ty::Adt(d2, ys)) => {
                d1 == d2 && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2).all(|(x, y)| self.unify(x, y))
                    && self.unify(r1, r2)
            }
            (Ty::Param(x), Ty::Param(y)) => x == y,
            (Ty::Dyn(x), Ty::Dyn(y)) => x == y,
            _ => false,
        }
    }

    /// Unifies without keeping any bindings if it fails.
    pub fn try_unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let snapshot = self.vars.clone();
        let ok = self.unify(a, b);
        if !ok {
            self.vars = snapshot;
        }
        ok
    }

    /// Whether `a` and `b` would unify, without changing any bindings.
    pub fn can_unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let snapshot = self.vars.clone();
        let ok = self.unify(a, b);
        self.vars = snapshot;
        ok
    }

    /// Binds every unbound integer variable to `i32` and float variable to `f64`.
    pub fn apply_literal_defaults(&mut self) {
        for i in 0..self.vars.len() {
            if self.vars[i].0.is_none() {
                match self.vars[i].1 {
                    VarKind::Int => self.vars[i].0 = Some(Ty::Prim(PrimTy::I32)),
                    VarKind::Float => self.vars[i].0 = Some(Ty::Prim(PrimTy::F64)),
                    VarKind::General => {}
                }
            }
        }
    }

    /// Whether `t` still contains an unbound general variable.
    pub fn has_unbound(&self, t: &Ty) -> bool {
        match self.zonk(t) {
            Ty::Var(_) => true,
            Ty::Tuple(ts) | Ty::Adt(_, ts) => ts.iter().any(|x| self.has_unbound(x)),
            Ty::Fn(ps, r) => ps.iter().any(|x| self.has_unbound(x)) || self.has_unbound(&r),
            Ty::Ref(_, x)
            | Ty::Ptr(_, x)
            | Ty::Array(x)
            | Ty::Slice(x)
            | Ty::ChanSend(x)
            | Ty::ChanRecv(x) => self.has_unbound(&x),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unify_binds_and_zonks() {
        let mut t = InferTable::default();
        let v = t.fresh(VarKind::General);
        assert!(t.unify(
            &Ty::Tuple(vec![v.clone(), Ty::Unit]),
            &Ty::Tuple(vec![Ty::Prim(PrimTy::Bool), Ty::Unit])
        ));
        assert_eq!(t.zonk(&v), Ty::Prim(PrimTy::Bool));
    }

    #[test]
    fn int_vars_only_bind_to_ints() {
        let mut t = InferTable::default();
        let v = t.fresh(VarKind::Int);
        assert!(!t.unify(&v, &Ty::Prim(PrimTy::Bool)));
        assert!(t.unify(&v, &Ty::Prim(PrimTy::U8)));
        let f = t.fresh(VarKind::Float);
        let i = t.fresh(VarKind::Int);
        assert!(!t.unify(&f, &i));
    }

    #[test]
    fn literal_defaults() {
        let mut t = InferTable::default();
        let i = t.fresh(VarKind::Int);
        let f = t.fresh(VarKind::Float);
        t.apply_literal_defaults();
        assert_eq!(t.zonk(&i), Ty::Prim(PrimTy::I32));
        assert_eq!(t.zonk(&f), Ty::Prim(PrimTy::F64));
    }

    #[test]
    fn occurs_check() {
        let mut t = InferTable::default();
        let v = t.fresh(VarKind::General);
        assert!(!t.unify(&v, &Ty::Tuple(vec![v.clone()])));
    }

    #[test]
    fn try_unify_rolls_back() {
        let mut t = InferTable::default();
        let v = t.fresh(VarKind::General);
        assert!(!t.try_unify(
            &Ty::Tuple(vec![v.clone(), Ty::Unit]),
            &Ty::Tuple(vec![Ty::Unit, Ty::Never])
        ));
        assert!(matches!(t.shallow(&v), Ty::Var(_)));
    }

    #[test]
    fn unknown_unifies_with_anything() {
        let mut t = InferTable::default();
        assert!(t.unify(&Ty::Unknown, &Ty::Prim(PrimTy::Char)));
        assert!(t.unify(&Ty::Adt(DefId(1), vec![]), &Ty::Unknown));
    }
}
