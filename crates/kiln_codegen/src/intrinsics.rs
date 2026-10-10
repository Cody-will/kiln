//! Compiler-provided operations: prelude functions without bodies
//! (`String::from`, `Vec::push`, `f64::sqrt`, `panic`, ...) and the MIR
//! intrinsics (`clone`, `drop`, `len`, `vec_take`, ...).

use crate::func::FnCx;
use crate::Codegen;
use cranelift_codegen::ir::{condcodes::IntCC, types, InstBuilder, Value};
use kiln_mir::Operand;
use kiln_parser::ast::TypeKind;
use kiln_typeck::ty::Ty;

/// `Type::method` for a prelude method, or the function's name.
pub(crate) fn qualified_name(cg: &Codegen<'_, '_>, idx: usize) -> String {
    let f = &cg.r.fns[idx];
    match f.impl_idx {
        Some(i) => {
            let ty = match &cg.r.impls[i].decl.self_ty.kind {
                TypeKind::Path(p) => p
                    .segments
                    .last()
                    .map(|s| s.ident.name.clone())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            format!("{ty}::{}", f.decl.name.name)
        }
        None => f.decl.name.name.clone(),
    }
}

impl FnCx<'_, '_, '_, '_> {
    fn bool_out(&mut self, v: Value, dest: Value) {
        let b = self.b.ins().icmp_imm(IntCC::NotEqual, v, 0);
        self.store(b, dest, 0);
    }

    /// `(element type, size, align)` of a `Vec<T>` type (looking through references).
    fn vec_elem(&mut self, mut t: Ty) -> (Ty, u32, u32) {
        while let Ty::Ref(_, x) = t {
            t = *x;
        }
        let et = match &t {
            Ty::Adt(_, a) if !a.is_empty() => a[0].clone(),
            Ty::Slice(x) => (**x).clone(),
            _ => Ty::Unit,
        };
        let l = self.cg.layout(&et);
        (et, l.size, l.align)
    }

    pub(crate) fn intrinsic(&mut self, name: &str, args: &[Operand], dest: Value, dt: &Ty) {
        let short = name.rsplit("::").next().unwrap_or(name);
        let ty_name = name.split("::").next().unwrap_or("");
        match (ty_name, short) {
            ("String", "new") => self.init_string(dest),
            ("String", "from") | ("str", "to_string") => {
                let (p, n) = self.str_of(&args[0]);
                self.call_rt("kn_string_from", &[dest, p, n]);
            }
            ("String" | "str", "len") => {
                let (_, n) = self.str_of(&args[0]);
                self.store(n, dest, 0);
            }
            ("String" | "str", "is_empty") => {
                let (_, n) = self.str_of(&args[0]);
                let z = self.b.ins().icmp_imm(IntCC::Equal, n, 0);
                self.store(z, dest, 0);
            }
            ("String", "push") => {
                let s = self.op_scalar(&args[0]);
                let (p, n) = self.str_of(&args[1]);
                self.call_rt("kn_string_push", &[s, p, n]);
            }
            ("String", "as_str") => {
                let (p, n) = self.str_of(&args[0]);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            ("String" | "str", "to_upper") => {
                let (p, n) = self.str_of(&args[0]);
                self.call_rt("kn_str_to_upper", &[dest, p, n]);
            }
            ("String" | "str", "to_lower") => {
                let (p, n) = self.str_of(&args[0]);
                self.call_rt("kn_str_to_lower", &[dest, p, n]);
            }
            ("String" | "str", "trim") => {
                let (p, n) = self.str_of(&args[0]);
                self.call_rt("kn_str_trim", &[dest, p, n]);
            }
            ("str", "starts_with" | "ends_with" | "contains") => {
                let (p, n) = self.str_of(&args[0]);
                let (q, m) = self.str_of(&args[1]);
                let f = match short {
                    "starts_with" => "kn_str_starts_with",
                    "ends_with" => "kn_str_ends_with",
                    _ => "kn_str_contains",
                };
                let r = self.call_rt(f, &[p, n, q, m]).unwrap();
                self.bool_out(r, dest);
            }
            ("Vec", "new" | "with_capacity") => {
                let one = self.iconst(8);
                let z = self.iconst(0);
                self.store(one, dest, 0);
                self.store(z, dest, 8);
                self.store(z, dest, 16);
                if short == "with_capacity" {
                    let n = self.op_scalar(&args[0]);
                    let (_, s, a) = self.vec_elem(dt.clone());
                    let (s, a) = (self.iconst(s as i64), self.iconst(a as i64));
                    self.call_rt("kn_vec_reserve", &[dest, n, s, a]);
                }
            }
            ("Vec", "len") => {
                let v = self.op_scalar(&args[0]);
                let n = self.load(types::I64, v, 16);
                self.store(n, dest, 0);
            }
            ("Vec", "is_empty") => {
                let v = self.op_scalar(&args[0]);
                let n = self.load(types::I64, v, 16);
                let z = self.b.ins().icmp_imm(IntCC::Equal, n, 0);
                self.store(z, dest, 0);
            }
            ("Vec", "push") => {
                let vt = self.op_ty(&args[0]);
                let v = self.op_scalar(&args[0]);
                let (_, size, align) = self.vec_elem(vt);
                let (one, s, a) = (
                    self.iconst(1),
                    self.iconst(size as i64),
                    self.iconst(align as i64),
                );
                self.call_rt("kn_vec_reserve", &[v, one, s, a]);
                let data = self.load(types::I64, v, 0);
                let len = self.load(types::I64, v, 16);
                let off = self.b.ins().imul_imm(len, size as i64);
                let at = self.b.ins().iadd(data, off);
                self.store_op(&args[1], at);
                let len2 = self.b.ins().iadd_imm(len, 1);
                self.store(len2, v, 16);
            }
            ("Vec", "pop") => {
                let vt = self.op_ty(&args[0]);
                let v = self.op_scalar(&args[0]);
                let (_, size, _) = self.vec_elem(vt);
                let (poff, _) = self.cg.field(dt, "Some.0");
                let len = self.load(types::I64, v, 16);
                let none = self.b.ins().iconst(types::I32, 1);
                self.store(none, dest, 0);
                let has = self.b.ins().icmp_imm(IntCC::NotEqual, len, 0);
                self.when(has, |cx| {
                    let len1 = cx.b.ins().iadd_imm(len, -1);
                    cx.store(len1, v, 16);
                    let data = cx.load(types::I64, v, 0);
                    let off = cx.b.ins().imul_imm(len1, size as i64);
                    let src = cx.b.ins().iadd(data, off);
                    let d = cx.b.ins().iadd_imm(dest, poff as i64);
                    cx.copy(d, src, size);
                    let some = cx.b.ins().iconst(types::I32, 0);
                    cx.store(some, dest, 0);
                });
            }
            ("Vec", "get" | "first" | "last") => {
                let vt = self.op_ty(&args[0]);
                let v = self.op_scalar(&args[0]);
                let (_, size, _) = self.vec_elem(vt);
                let len = self.load(types::I64, v, 16);
                let i = match short {
                    "get" => {
                        let (i, _) = self.scalar_through_refs(&args[1]);
                        i
                    }
                    "first" => self.iconst(0),
                    _ => self.b.ins().iadd_imm(len, -1),
                };
                let (poff, _) = self.cg.field(dt, "Some.0");
                let none = self.b.ins().iconst(types::I32, 1);
                self.store(none, dest, 0);
                let ok = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                self.when(ok, |cx| {
                    let data = cx.load(types::I64, v, 0);
                    let off = cx.b.ins().imul_imm(i, size as i64);
                    let p = cx.b.ins().iadd(data, off);
                    cx.store(p, dest, poff as i32);
                    let some = cx.b.ins().iconst(types::I32, 0);
                    cx.store(some, dest, 0);
                });
            }
            ("Vec", "clear") => {
                let vt = self.op_ty(&args[0]);
                let v = self.op_scalar(&args[0]);
                let (et, size, _) = self.vec_elem(vt);
                if self.cg.needs_drop(&et) {
                    let data = self.load(types::I64, v, 0);
                    let len = self.load(types::I64, v, 16);
                    // Drop each element (simple counted loop).
                    let i = self.slot(8, 8);
                    let z = self.iconst(0);
                    self.store(z, i, 0);
                    let (head, body, done) = (
                        self.b.create_block(),
                        self.b.create_block(),
                        self.b.create_block(),
                    );
                    self.b.ins().jump(head, &[]);
                    self.b.switch_to_block(head);
                    let iv = self.load(types::I64, i, 0);
                    let more = self.b.ins().icmp(IntCC::UnsignedLessThan, iv, len);
                    self.b.ins().brif(more, body, &[], done, &[]);
                    self.b.switch_to_block(body);
                    let off = self.b.ins().imul_imm(iv, size as i64);
                    let e = self.b.ins().iadd(data, off);
                    self.drop_value(e, &et);
                    let n = self.b.ins().iadd_imm(iv, 1);
                    self.store(n, i, 0);
                    self.b.ins().jump(head, &[]);
                    self.b.switch_to_block(done);
                }
                let z = self.iconst(0);
                self.store(z, v, 16);
            }
            ("Box", "new") => {
                let t = self.op_ty(&args[0]);
                let l = self.cg.layout(&t);
                let (s, a) = (self.iconst(l.size as i64), self.iconst(l.align as i64));
                let p = self.call_rt("kn_alloc", &[s, a]).unwrap();
                self.store_op(&args[0], p);
                self.store(p, dest, 0);
            }
            ("f64" | "f32", m) => {
                let (v, _) = self.scalar_through_refs(&args[0]);
                let r = match m {
                    "sqrt" => self.b.ins().sqrt(v),
                    "abs" => self.b.ins().fabs(v),
                    "floor" => self.b.ins().floor(v),
                    "ceil" => self.b.ins().ceil(v),
                    "round" => self.call_rt("kn_round", &[v]).unwrap(),
                    "powi" => {
                        let (n, _) = self.scalar_through_refs(&args[1]);
                        self.call_rt("kn_powi", &[v, n]).unwrap()
                    }
                    "min" | "max" => {
                        let (w, _) = self.scalar_through_refs(&args[1]);
                        if m == "min" {
                            self.b.ins().fmin(v, w)
                        } else {
                            self.b.ins().fmax(v, w)
                        }
                    }
                    _ => {
                        return self.error(format!("code generation does not support `{name}` yet"))
                    }
                };
                self.store(r, dest, 0);
            }
            (_, "panic") if args.len() == 1 => {
                let (p, n) = self.str_of(&args[0]);
                self.call_rt("kn_panic", &[p, n]);
            }
            (_, "assert") => {
                let c = self.op_scalar(&args[0]);
                let bad = self.b.ins().icmp_imm(IntCC::Equal, c, 0);
                self.when(bad, |cx| cx.panic_msg("assertion failed"));
            }
            (_, "drop") => {
                let t = self.op_ty(&args[0]);
                let s = self.slot_for(&t);
                self.store_op(&args[0], s);
                self.drop_value(s, &t);
            }
            (_, "clone") => {
                let rt = self.op_ty(&args[0]);
                if let Ty::Ref(..) = dt {
                    // Cloning a reference copies it.
                    let a = self.op_addr(&args[0]);
                    let src = self.load(types::I64, a, 0);
                    let size = self.cg.layout(dt).size;
                    self.copy(dest, src, size);
                    return;
                }
                let _ = rt;
                let src = self.op_scalar(&args[0]);
                self.clone_into(src, dest, dt);
            }
            (_, "len") => {
                let t = self.op_ty(&args[0]);
                let a = self.op_addr(&args[0]);
                let n = match &t {
                    Ty::Array(_, n) => self.iconst(*n as i64),
                    Ty::Ref(_, x) if matches!(**x, Ty::Array(..)) => match &**x {
                        Ty::Array(_, n) => self.iconst(*n as i64),
                        _ => unreachable!(),
                    },
                    Ty::Ref(_, x) if matches!(**x, Ty::Slice(_)) => self.load(types::I64, a, 8),
                    Ty::Ref(..) => {
                        let p = self.load(types::I64, a, 0);
                        self.load(types::I64, p, 16)
                    }
                    _ => self.load(types::I64, a, 16),
                };
                self.store(n, dest, 0);
            }
            (_, "vec_take") => {
                let t = self.op_ty(&args[0]);
                let (_, size, _) = self.vec_elem(t);
                let Some(p) = args[0].place() else { return };
                let (a, _) = self.place_addr(p);
                let (i, _) = self.scalar_through_refs(&args[1]);
                let data = self.load(types::I64, a, 0);
                let off = self.b.ins().imul_imm(i, size as i64);
                let src = self.b.ins().iadd(data, off);
                self.copy(dest, src, size);
            }
            (_, "array_iter_view") => {
                // A non-owning view of an array's storage (which stays in place).
                let t = self.op_ty(&args[0]);
                let Some(p) = args[0].place() else { return };
                let (a, _) = self.place_addr(p);
                let n = match t {
                    Ty::Array(_, n) => n,
                    _ => 0,
                };
                let not_owned = self.iconst(-1);
                let len = self.iconst(n as i64);
                self.store(a, dest, 0);
                self.store(not_owned, dest, 8);
                self.store(len, dest, 16);
                self.moved(p);
            }
            (_, "vec_iter_view") => {
                let Some(p) = args[0].place() else { return };
                let (a, _) = self.place_addr(p);
                let data = self.load(types::I64, a, 0);
                let len = self.load(types::I64, a, 16);
                let not_owned = self.iconst(-1);
                self.store(data, dest, 0);
                self.store(not_owned, dest, 8);
                self.store(len, dest, 16);
            }
            (_, "vec_take_front") => {
                let t = self.op_ty(&args[0]);
                let (_, size, _) = self.vec_elem(t);
                let Some(p) = args[0].place() else { return };
                let (a, _) = self.place_addr(p);
                let data = self.load(types::I64, a, 0);
                self.copy(dest, data, size);
                let next = self.b.ins().iadd_imm(data, size as i64);
                self.store(next, a, 0);
                let len = self.load(types::I64, a, 16);
                let len1 = self.b.ins().iadd_imm(len, -1);
                self.store(len1, a, 16);
            }
            (_, "vec_forget_elems") => {
                let Some(p) = args[0].place() else { return };
                let (a, _) = self.place_addr(p);
                let z = self.iconst(0);
                self.store(z, a, 16);
            }
            (_, "display_to_string") => {
                let t = self.op_ty(&args[0]);
                let inner = match &t {
                    Ty::Ref(_, x) => (**x).clone(),
                    t => t.clone(),
                };
                let a = self.op_addr(&args[0]);
                let p = self.load(types::I64, a, 0);
                self.init_string(dest);
                self.fmt_into(dest, p, &inner);
            }
            _ => self.error(format!("code generation does not support `{name}` yet")),
        }
    }
}
