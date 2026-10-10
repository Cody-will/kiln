//! Memory layout of Kiln types (64-bit targets).
//!
//! * Integers, floats, `bool` (1 byte), `char` (4 bytes): scalars.
//! * `&T`, `*T`, `Box<T>`: one pointer; when `T` is unsized (`str`, `[T]`,
//!   `dyn Trait`) a fat pointer `(data, metadata)` where metadata is the length
//!   or the vtable.
//! * `String`, `Vec<T>`: `{ ptr, cap, len }` (shared with the C runtime).
//! * Structs and tuples: fields in declaration order, each aligned.
//! * Enums: a `u32` tag at offset 0, then the active variant's fields starting
//!   at a common payload offset.
//! * Function values (closures, fn items as values): `{ code, env }`.
//! * `Range<T>`: `{ start, end, incl: u8 }` (`incl`: 0 for `a..b`, 1 for
//!   `a..=b`, 2 once an inclusive range is used up).

use crate::Codegen;
use cranelift_codegen::ir::{types, Type};
use kiln_resolve::{DefKind, PrimTy};
use kiln_typeck::ty::Ty;

/// Size and alignment of a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Size in bytes (a multiple of `align`).
    pub size: u32,
    /// Alignment in bytes.
    pub align: u32,
}

pub(crate) fn align_up(x: u32, a: u32) -> u32 {
    x.div_ceil(a) * a
}

/// Offsets of a list of fields laid out in order, plus the total layout.
pub(crate) fn sequential(fields: &[Layout]) -> (Vec<u32>, Layout) {
    let mut off = 0;
    let mut align = 1;
    let mut offs = vec![];
    for f in fields {
        off = align_up(off, f.align);
        offs.push(off);
        off += f.size;
        align = align.max(f.align);
    }
    (
        offs,
        Layout {
            size: align_up(off, align),
            align,
        },
    )
}

impl Codegen<'_, '_> {
    /// Whether `t` is dynamically sized.
    pub(crate) fn is_unsized(&self, t: &Ty) -> bool {
        matches!(t, Ty::Prim(PrimTy::Str) | Ty::Slice(_) | Ty::Dyn(_))
    }

    pub(crate) fn is_lang(&self, t: &Ty, name: &str) -> bool {
        matches!(t, Ty::Adt(d, _) if self.r.prelude_type(name) == Some(*d))
    }

    /// The pointee of a pointer-like type (`&T`, `*T`, `Box<T>`).
    pub(crate) fn pointee(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Ref(_, x) | Ty::Ptr(_, x) => Some((**x).clone()),
            Ty::Adt(_, a) if self.is_lang(t, "Box") => Some(a[0].clone()),
            _ => None,
        }
    }

    /// Whether `t` is a fat pointer.
    pub(crate) fn is_fat(&self, t: &Ty) -> bool {
        self.pointee(t).is_some_and(|p| self.is_unsized(&p))
    }

    /// The Cranelift type of a scalar, or `None` for aggregates.
    pub(crate) fn scalar(&self, t: &Ty) -> Option<Type> {
        Some(match t {
            Ty::Prim(p) => match p {
                PrimTy::I8 | PrimTy::U8 | PrimTy::Bool => types::I8,
                PrimTy::I16 | PrimTy::U16 => types::I16,
                PrimTy::I32 | PrimTy::U32 | PrimTy::Char => types::I32,
                PrimTy::I64 | PrimTy::U64 | PrimTy::Isize | PrimTy::Usize => types::I64,
                PrimTy::I128 | PrimTy::U128 => types::I128,
                PrimTy::F32 => types::F32,
                PrimTy::F64 => types::F64,
                PrimTy::Str => return None,
            },
            Ty::Ref(..) | Ty::Ptr(..) if !self.is_fat(t) => types::I64,
            Ty::Adt(..) if self.is_lang(t, "Box") && !self.is_fat(t) => types::I64,
            Ty::ChanSend(_) | Ty::ChanRecv(_) => types::I64,
            _ => return None,
        })
    }

    /// Size and alignment of `t`.
    pub(crate) fn layout(&mut self, t: &Ty) -> Layout {
        let key = format!("{t:?}");
        if let Some(l) = self.layouts.get(&key) {
            return *l;
        }
        let l = self.compute_layout(t);
        self.layouts.insert(key, l);
        l
    }

    fn compute_layout(&mut self, t: &Ty) -> Layout {
        let word = Layout { size: 8, align: 8 };
        if let Some(s) = self.scalar(t) {
            let n = s.bytes();
            return Layout {
                size: n,
                align: n.min(8),
            };
        }
        match t {
            Ty::Unit | Ty::Never => Layout { size: 0, align: 1 },
            Ty::Ref(..) | Ty::Ptr(..) => Layout { size: 16, align: 8 },
            Ty::Fn(..) => Layout { size: 16, align: 8 },
            Ty::Tuple(ts) => {
                let ls: Vec<Layout> = ts.iter().map(|x| self.layout(x)).collect();
                sequential(&ls).1
            }
            Ty::Adt(..) if self.is_lang(t, "Box") => Layout { size: 16, align: 8 },
            // Channel ends and task handles point to runtime objects.
            Ty::ChanSend(_) | Ty::ChanRecv(_) => word,
            Ty::Adt(..) if self.is_lang(t, "Task") => word,
            Ty::Adt(..) if self.is_lang(t, "String") || self.is_lang(t, "Vec") => {
                Layout { size: 24, align: 8 }
            }
            Ty::Adt(_, a) if self.is_lang(t, "Range") => {
                let e = self.layout(&a[0]);
                sequential(&[e, e, Layout { size: 1, align: 1 }]).1
            }
            Ty::Adt(d, _) => match self.r.def(*d).kind {
                DefKind::Enum => {
                    let (_, l) = self.enum_layout(t);
                    l
                }
                _ => {
                    let fs: Vec<Layout> = self
                        .typed
                        .adt_fields(t, None)
                        .iter()
                        .map(|(_, ft)| self.layout(ft))
                        .collect();
                    sequential(&fs).1
                }
            },
            Ty::Prim(PrimTy::Str) | Ty::Slice(_) | Ty::Dyn(_) => Layout { size: 0, align: 1 },
            Ty::Array(x, n) => {
                let e = self.layout(x);
                Layout {
                    size: e.size.saturating_mul(*n as u32),
                    align: e.align,
                }
            }
            _ => {
                self.layout_error(t);
                word
            }
        }
    }

    fn layout_error(&mut self, t: &Ty) {
        let name = self.typed.fmt_ty(t);
        self.fatal(format!(
            "code generation does not support values of type `{name}` yet"
        ));
    }

    /// Payload offset and total layout of an enum.
    pub(crate) fn enum_layout(&mut self, t: &Ty) -> (u32, Layout) {
        let Ty::Adt(d, _) = t else {
            return (0, Layout { size: 0, align: 1 });
        };
        let mut max_align = 4;
        let mut payloads = vec![];
        for v in self.r.variants(*d) {
            let fs: Vec<Layout> = self
                .typed
                .adt_fields(t, Some(v))
                .iter()
                .map(|(_, ft)| self.layout(ft))
                .collect();
            let (_, l) = sequential(&fs);
            max_align = max_align.max(l.align);
            payloads.push(l.size);
        }
        let off = align_up(4, max_align);
        let size = align_up(off + payloads.into_iter().max().unwrap_or(0), max_align);
        (
            off,
            Layout {
                size,
                align: max_align,
            },
        )
    }

    /// Offset and type of field `name` of `t` (`"0"` for tuples, `"Variant.f"` for enum fields).
    pub(crate) fn field(&mut self, t: &Ty, name: &str) -> (u32, Ty) {
        match t {
            Ty::Tuple(ts) => {
                let i: usize = name.parse().unwrap_or(0);
                let ls: Vec<Layout> = ts.iter().map(|x| self.layout(x)).collect();
                let (offs, _) = sequential(&ls);
                (
                    offs.get(i).copied().unwrap_or(0),
                    ts.get(i).cloned().unwrap_or(Ty::Unit),
                )
            }
            Ty::Adt(_, a) if self.is_lang(t, "Range") => {
                let e = self.layout(&a[0]);
                match name {
                    "end" => (align_up(e.size, e.align), a[0].clone()),
                    "incl" => (align_up(e.size, e.align) + e.size, Ty::Prim(PrimTy::U8)),
                    _ => (0, a[0].clone()),
                }
            }
            Ty::Adt(d, _) => {
                if let Some((vname, f)) = name.split_once('.') {
                    let (poff, _) = self.enum_layout(t);
                    let v = self
                        .r
                        .variants(*d)
                        .into_iter()
                        .find(|v| self.r.def(*v).name == vname);
                    let fields = self.typed.adt_fields(t, v);
                    let ls: Vec<Layout> = fields.iter().map(|(_, ft)| self.layout(ft)).collect();
                    let (offs, _) = sequential(&ls);
                    let i = fields.iter().position(|(n, _)| n == f).unwrap_or(0);
                    return (
                        poff + offs.get(i).copied().unwrap_or(0),
                        fields.get(i).map(|x| x.1.clone()).unwrap_or(Ty::Unit),
                    );
                }
                let fields = self.typed.adt_fields(t, None);
                let ls: Vec<Layout> = fields.iter().map(|(_, ft)| self.layout(ft)).collect();
                let (offs, _) = sequential(&ls);
                let i = fields.iter().position(|(n, _)| n == name).unwrap_or(0);
                (
                    offs.get(i).copied().unwrap_or(0),
                    fields.get(i).map(|x| x.1.clone()).unwrap_or(Ty::Unit),
                )
            }
            _ => (0, Ty::Unit),
        }
    }

    /// Whether dropping a `t` runs any code.
    pub(crate) fn needs_drop(&mut self, t: &Ty) -> bool {
        let key = format!("{t:?}");
        if let Some(b) = self.needs_drop_cache.get(&key) {
            return *b;
        }
        self.needs_drop_cache.insert(key.clone(), false); // cycle guard
        let b = match t {
            Ty::Adt(..)
                if self.is_lang(t, "String")
                    || self.is_lang(t, "Vec")
                    || self.is_lang(t, "Box") =>
            {
                true
            }
            Ty::Adt(..) if self.is_lang(t, "Range") => false,
            Ty::Adt(..) if self.is_lang(t, "Task") => true,
            Ty::ChanSend(_) | Ty::ChanRecv(_) => true,
            Ty::Adt(d, _) => {
                self.typed.has_drop_impl(t) || {
                    let variants: Vec<Option<kiln_resolve::DefId>> = match self.r.def(*d).kind {
                        DefKind::Enum => self.r.variants(*d).into_iter().map(Some).collect(),
                        _ => vec![None],
                    };
                    variants.into_iter().any(|v| {
                        let fs = self.typed.adt_fields(t, v);
                        fs.iter().any(|(_, ft)| self.needs_drop(ft))
                    })
                }
            }
            Ty::Tuple(ts) => ts.iter().any(|x| self.needs_drop(x)),
            Ty::Array(x, n) => *n > 0 && self.needs_drop(x),
            _ => false,
        };
        self.needs_drop_cache.insert(key, b);
        b
    }
}
