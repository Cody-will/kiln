//! FFI checks: `@repr(C)` structs and `extern "C"` function signatures
//! may only use types with a C equivalent.

use crate::collect::{Checker, Shape};
use crate::ty::Ty;
use kiln_resolve::{DefId, DefKind, PrimTy};

/// Whether `item` carries `@repr(C)`.
pub(crate) fn is_repr_c(item: &kiln_parser::ast::Item) -> bool {
    item.attrs
        .iter()
        .any(|a| a.name.name == "repr" && a.args.as_deref().map(str::trim) == Some("C"))
}

impl Checker<'_, '_> {
    fn repr_c(&self, d: DefId) -> bool {
        self.r.def(d).item.is_some_and(is_repr_c)
    }

    /// Why `t` has no C equivalent, or `None` if it does. `by_value`
    /// rejects structs (foreign functions take and return scalars and
    /// pointers; pass a `*const T` to a `@repr(C)` struct).
    fn not_ffi(&self, t: &Ty, by_value: bool) -> Option<String> {
        match t {
            Ty::Prim(PrimTy::Str) => {
                Some("`str` has no C equivalent; pass `*const u8` and a length".into())
            }
            Ty::Prim(PrimTy::I128 | PrimTy::U128) => {
                Some("128-bit integers have no stable C ABI".into())
            }
            Ty::Prim(_) | Ty::Ptr(..) => None,
            Ty::Unit => None,
            Ty::Array(x, _) if !by_value => self.not_ffi(x, false),
            Ty::Adt(d, _) if self.repr_c(*d) => by_value.then(|| {
                "structs are passed to C through pointers here: use `*const T` / `*mut T`".into()
            }),
            Ty::Adt(d, _) => Some(format!("`{}` is not `@repr(C)`", self.r.def(*d).name)),
            Ty::Ref(..) => {
                Some("references are Kiln-only; use a raw pointer (`r as *const T`)".into())
            }
            Ty::Fn(..) => Some("Kiln function values are not C function pointers".into()),
            _ => Some("this type has no C equivalent".into()),
        }
    }

    pub(crate) fn check_ffi(&mut self) {
        let mut errs = vec![];
        for (i, d) in self.r.defs.iter().enumerate() {
            let Some(item) = d.item else { continue };
            if !is_repr_c(item) {
                continue;
            }
            if d.kind != DefKind::Struct {
                errs.push(("`@repr(C)` applies to structs".to_string(), d.span));
                continue;
            }
            let id = DefId(i as u32);
            let Some(info) = self.adts.get(&id) else {
                continue;
            };
            if !info.generics.is_empty() {
                errs.push(("`@repr(C)` structs cannot be generic".into(), d.span));
                continue;
            }
            let fields: Vec<(String, Ty)> = match &info.shape {
                Shape::Named(fs) => fs.clone(),
                Shape::Tuple(ts) => ts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (i.to_string(), t.clone()))
                    .collect(),
                Shape::Unit => vec![],
            };
            if fields.is_empty() {
                errs.push(("a `@repr(C)` struct needs at least one field (empty C structs are not portable)".into(), d.span));
            }
            for (n, t) in fields {
                if let Some(why) = self.not_ffi(&t, false) {
                    errs.push((
                        format!("field `{n}` of `@repr(C)` struct `{}`: {why}", d.name),
                        d.span,
                    ));
                }
            }
        }
        for (i, f) in self.r.fns.iter().enumerate() {
            if !f.item.attrs.iter().any(|a| a.name.name == "extern") {
                continue;
            }
            let sig = &self.sigs[i];
            let name = &f.decl.name.name;
            for (k, t) in sig.params.iter().enumerate() {
                if let Some(why) = self.not_ffi(t, true) {
                    let span = f.decl.params.get(k).map_or(f.decl.name.span, |p| p.span);
                    errs.push((
                        format!("parameter of foreign function `{name}`: {why}"),
                        span,
                    ));
                }
            }
            if let Some(why) = self.not_ffi(&sig.ret, true) {
                errs.push((
                    format!("result of foreign function `{name}`: {why}"),
                    f.decl.name.span,
                ));
            }
        }
        for (m, s) in errs {
            self.error(m, s);
        }
    }
}
