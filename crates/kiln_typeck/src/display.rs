//! Rendering types for diagnostics.

use crate::collect::Checker;
use crate::ty::{Ty, VarKind};
use kiln_resolve::PrimTy;

impl Checker<'_, '_> {
    /// Renders a type as Kiln source, e.g. `Option<&str>` or `{integer}`.
    pub(crate) fn fmt_ty(&self, t: &Ty) -> String {
        let t = self.infer.zonk(t);
        match &t {
            Ty::Prim(p) => prim_name(*p).to_string(),
            Ty::Unit => "void".into(),
            Ty::Never => "!".into(),
            Ty::Tuple(ts) => {
                let inner: Vec<_> = ts.iter().map(|x| self.fmt_ty(x)).collect();
                if ts.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
            Ty::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, self.fmt_ty(x)),
            Ty::Ptr(m, x) => format!("*{} {}", if *m { "mut" } else { "const" }, self.fmt_ty(x)),
            Ty::Array(x, n) => format!("[{}; {n}]", self.fmt_ty(x)),
            Ty::Slice(x) => format!("[{}]", self.fmt_ty(x)),
            Ty::Adt(d, args) => {
                let name = &self.r.def(*d).name;
                if args.is_empty() {
                    name.clone()
                } else {
                    let a: Vec<_> = args.iter().map(|x| self.fmt_ty(x)).collect();
                    format!("{name}<{}>", a.join(", "))
                }
            }
            Ty::Fn(ps, r, is_mut) => {
                let a: Vec<_> = ps.iter().map(|x| self.fmt_ty(x)).collect();
                let kw = if *is_mut { "fun mut" } else { "fun" };
                match self.infer.zonk(r) {
                    Ty::Unit => format!("{kw}({})", a.join(", ")),
                    r => format!("{kw}({}) {}", a.join(", "), self.fmt_ty(&r)),
                }
            }
            Ty::Param(n) => n.clone(),
            Ty::Dyn(ts) => {
                let a: Vec<_> = ts.iter().map(|d| self.r.def(*d).name.clone()).collect();
                format!("dyn {}", a.join(" + "))
            }
            Ty::ChanSend(x) => format!("chan<- {}", self.fmt_ty(x)),
            Ty::ChanRecv(x) => format!("<-chan {}", self.fmt_ty(x)),
            Ty::Var(v) => match self.infer.kind(*v) {
                VarKind::Int => "{integer}".into(),
                VarKind::Float => "{float}".into(),
                VarKind::General => "_".into(),
            },
            Ty::Unknown => "{unknown}".into(),
        }
    }
}

/// Source name of a primitive type.
pub(crate) fn prim_name(p: PrimTy) -> &'static str {
    use PrimTy::*;
    match p {
        I8 => "i8",
        I16 => "i16",
        I32 => "i32",
        I64 => "i64",
        I128 => "i128",
        Isize => "isize",
        U8 => "u8",
        U16 => "u16",
        U32 => "u32",
        U64 => "u64",
        U128 => "u128",
        Usize => "usize",
        F32 => "f32",
        F64 => "f64",
        Bool => "bool",
        Char => "char",
        Str => "str",
    }
}
