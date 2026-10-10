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
        // Free functions in `std` are qualified by their module (`mem::size_of`).
        None if f.module != cg.r.prelude => {
            format!("{}::{}", cg.r.def(f.module).name, f.decl.name.name)
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

    /// The runtime handle of a channel end (looking through references), and
    /// whether it is the sending end.
    fn chan_handle(&mut self, o: &Operand) -> Option<(Value, bool)> {
        let mut t = self.op_ty(o);
        if !matches!(t, Ty::Ref(..) | Ty::ChanSend(_) | Ty::ChanRecv(_)) {
            return None;
        }
        let mut a = self.op_addr(o);
        while let Ty::Ref(_, x) = t {
            a = self.load(types::I64, a, 0);
            t = *x;
        }
        let send = match t {
            Ty::ChanSend(_) => true,
            Ty::ChanRecv(_) => false,
            _ => return None,
        };
        Some((self.load(types::I64, a, 0), send))
    }

    /// Stores enum variant `index`'s tag (the payload is written separately).
    fn set_tag(&mut self, dest: Value, index: Value) {
        self.store(index, dest, 0);
    }

    /// Channel methods: `try_send`, `try_recv`, `recv`, `close`, `len`, `cap`, `clone`.
    fn chan_method(&mut self, short: &str, args: &[Operand], dest: Value, dt: &Ty) -> bool {
        let Some((h, send)) = args.first().and_then(|a| self.chan_handle(a)) else {
            return false;
        };
        match short {
            "recv" => {
                // Option<T>: Some = 0, None = 1.
                let (off, _) = self.cg.field(dt, "Some.0");
                let p = self.b.ins().iadd_imm(dest, off as i64);
                let ok = self.call_rt("kn_chan_recv", &[h, p]).unwrap();
                let tag = self.b.ins().bxor_imm(ok, 1);
                self.set_tag(dest, tag);
            }
            "try_recv" => {
                // Result<T, TryRecvError>: 0 received, 1 empty, 2 closed.
                let (off, _) = self.cg.field(dt, "Ok.0");
                let p = self.b.ins().iadd_imm(dest, off as i64);
                let r = self.call_rt("kn_chan_try_recv", &[h, p]).unwrap();
                let failed = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
                let tag = self.b.ins().uextend(types::I32, failed);
                self.set_tag(dest, tag);
                let dt2 = dt.clone();
                self.when(failed, |cx| {
                    let (eoff, _) = cx.cg.field(&dt2, "Err.0");
                    let e = cx.b.ins().iadd_imm(dest, eoff as i64);
                    let kind = cx.b.ins().iadd_imm(r, -1);
                    cx.set_tag(e, kind);
                });
            }
            "try_send" => {
                // Result<(), TrySendError<T>>: 0 sent, 1 full, 2 closed.
                let vt = self.op_ty(&args[1]);
                let size = self.cg.layout(&vt).size;
                let s = self.slot_for(&vt);
                self.store_op(&args[1], s);
                let r = self.call_rt("kn_chan_try_send", &[h, s]).unwrap();
                let failed = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
                let tag = self.b.ins().uextend(types::I32, failed);
                self.set_tag(dest, tag);
                let dt2 = dt.clone();
                self.when(failed, |cx| {
                    // The value comes back in the error.
                    let (eoff, et) = cx.cg.field(&dt2, "Err.0");
                    let e = cx.b.ins().iadd_imm(dest, eoff as i64);
                    let kind = cx.b.ins().iadd_imm(r, -1);
                    cx.set_tag(e, kind);
                    let (poff, _) = cx.cg.field(&et, "Full.0");
                    let p = cx.b.ins().iadd_imm(e, poff as i64);
                    cx.copy(p, s, size);
                });
            }
            "close" => {
                self.call_rt("kn_chan_close", &[h]);
            }
            "len" | "cap" => {
                let f = if short == "len" {
                    "kn_chan_len"
                } else {
                    "kn_chan_cap"
                };
                let n = self.call_rt(f, &[h]).unwrap();
                self.store(n, dest, 0);
            }
            "clone" => {
                let s = self.b.ins().iconst(types::I32, send as i64);
                self.call_rt("kn_chan_clone_end", &[h, s]);
                self.store(h, dest, 0);
            }
            _ => return false,
        }
        true
    }

    /// `select:<kinds>:<has default>` (see `kiln_mir`'s select lowering): the
    /// operands are (channel, receive buffer or sent value) per case.
    fn select(&mut self, name: &str, args: &[Operand], dest: Value) {
        let mut parts = name.split(':').skip(1);
        let kinds: Vec<char> = parts.next().unwrap_or("").chars().collect();
        let has_default = parts.next() == Some("1");
        let n = kinds.len();
        let cases = self.slot(24 * n.max(1) as u32, 8);
        for (i, k) in kinds.iter().enumerate() {
            let (h, _) = self.chan_handle(&args[2 * i]).unwrap_or_else(|| {
                let z = self.iconst(0);
                (z, false)
            });
            let elem = match args[2 * i + 1].place() {
                Some(p) => self.place_addr(p).0,
                None => self.iconst(0),
            };
            let send = self.iconst((*k == 's') as i64);
            let base = 24 * i as i32;
            self.store(h, cases, base);
            self.store(elem, cases, base + 8);
            self.store(send, cases, base + 16);
        }
        let (nv, dv) = (self.iconst(n as i64), self.iconst(has_default as i64));
        let r = self.call_rt("kn_select", &[cases, nv, dv]).unwrap();
        self.store(r, dest, 0);
    }

    /// The generic argument `T` of the intrinsic being generated.
    fn t_arg(&self) -> Ty {
        self.intr_substs.get("T").cloned().unwrap_or(Ty::Unit)
    }

    /// `std::mem`, `std::ptr`, and `Box` raw-pointer intrinsics. Returns false
    /// if `name` is not one of them.
    fn mem_intrinsic(&mut self, name: &str, args: &[Operand], dest: Value, dt: &Ty) -> bool {
        match name {
            "mem::size_of" | "mem::align_of" => {
                let t = self.t_arg();
                let l = self.cg.layout(&t);
                let v = if name == "mem::size_of" {
                    l.size
                } else {
                    l.align
                };
                let v = self.iconst(v as i64);
                self.store(v, dest, 0);
            }
            // The value was moved into the call; not dropping it is the point.
            "mem::forget" => {
                if let Operand::Move(p) = &args[0] {
                    self.moved(p);
                }
            }
            "mem::swap" => {
                let t = self.t_arg();
                let size = self.cg.layout(&t).size;
                let a = self.op_scalar(&args[0]);
                let b = self.op_scalar(&args[1]);
                let tmp = self.slot_for(&t);
                self.copy(tmp, a, size);
                self.copy(a, b, size);
                self.copy(b, tmp, size);
            }
            "mem::replace" => {
                let t = self.t_arg();
                let size = self.cg.layout(&t).size;
                let p = self.op_scalar(&args[0]);
                self.copy(dest, p, size);
                self.store_op(&args[1], p);
            }
            "ptr::null" => {
                let z = self.iconst(0);
                self.store(z, dest, 0);
            }
            "ptr::is_null" => {
                let p = self.op_scalar(&args[0]);
                let b = self.b.ins().icmp_imm(IntCC::Equal, p, 0);
                self.store(b, dest, 0);
            }
            "ptr::add" => {
                let t = self.t_arg();
                let size = self.cg.layout(&t).size as i64;
                let p = self.op_scalar(&args[0]);
                let n = self.op_scalar(&args[1]);
                let off = self.b.ins().imul_imm(n, size);
                let r = self.b.ins().iadd(p, off);
                self.store(r, dest, 0);
            }
            "ptr::read" => {
                let size = self.cg.layout(dt).size;
                let p = self.op_scalar(&args[0]);
                self.copy(dest, p, size);
            }
            "ptr::write" => {
                let p = self.op_scalar(&args[0]);
                self.store_op(&args[1], p);
            }
            "ptr::drop_in_place" => {
                let t = self.t_arg();
                let p = self.op_scalar(&args[0]);
                if self.cg.needs_drop(&t) {
                    self.drop_in_place(p, &t);
                }
            }
            "ptr::copy" => {
                let t = self.t_arg();
                let size = self.cg.layout(&t).size as i64;
                let src = self.op_scalar(&args[0]);
                let dst = self.op_scalar(&args[1]);
                let n = self.op_scalar(&args[2]);
                let bytes = self.b.ins().imul_imm(n, size);
                self.call_rt("kn_memmove", &[dst, src, bytes]);
            }
            // Raw parts of vectors, strings, and slices. A `String` has the
            // layout of a `Vec<u8>`: (data, capacity, length).
            "Vec::as_ptr" => {
                let v = self.op_scalar(&args[0]);
                let d = self.load(types::I64, v, 0);
                self.store(d, dest, 0);
            }
            "Vec::capacity" | "String::capacity" => {
                let v = self.op_scalar(&args[0]);
                let c = self.load(types::I64, v, 8);
                self.store(c, dest, 0);
            }
            "Vec::set_len" => {
                let v = self.op_scalar(&args[0]);
                let n = self.op_scalar(&args[1]);
                self.store(n, v, 16);
            }
            "Vec::reserve" | "String::reserve" => {
                let vt = self.op_ty(&args[0]);
                let v = self.op_scalar(&args[0]);
                let n = self.op_scalar(&args[1]);
                let (size, align) = if name == "String::reserve" {
                    (1, 1)
                } else {
                    let (_, s, a) = self.vec_elem(vt);
                    (s, a)
                };
                let (s, a) = (self.iconst(size as i64), self.iconst(align as i64));
                self.call_rt("kn_vec_reserve", &[v, n, s, a]);
            }
            "String::from_utf8_unchecked" | "String::into_bytes" => {
                let src = self.op_addr(&args[0]);
                self.copy(dest, src, 24);
                if let Operand::Move(p) = &args[0] {
                    self.moved(p);
                }
            }
            "String::as_mut_vec" => {
                let v = self.op_scalar(&args[0]);
                self.store(v, dest, 0);
            }
            "String::with_capacity" => {
                let one = self.iconst(1);
                let z = self.iconst(0);
                self.store(one, dest, 0);
                self.store(z, dest, 8);
                self.store(z, dest, 16);
                let n = self.op_scalar(&args[0]);
                let (s, a) = (self.iconst(1), self.iconst(1));
                self.call_rt("kn_vec_reserve", &[dest, n, s, a]);
            }
            "String::as_bytes" | "str::as_bytes" => {
                let (p, n) = self.str_of(&args[0]);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            "ptr::slice_ptr" | "ptr::str_ptr" => {
                let a = self.op_addr(&args[0]);
                let p = self.load(types::I64, a, 0);
                self.store(p, dest, 0);
            }
            "ptr::slice_from_raw" | "ptr::str_from_raw" => {
                let p = self.op_scalar(&args[0]);
                let n = self.op_scalar(&args[1]);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            "Box::into_raw" | "Box::from_raw" => {
                let p = self.op_scalar(&args[0]);
                self.store(p, dest, 0);
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn intrinsic(&mut self, name: &str, args: &[Operand], dest: Value, dt: &Ty) {
        let short = name.rsplit("::").next().unwrap_or(name);
        let ty_name = name.split("::").next().unwrap_or("");
        if name.starts_with("select:") {
            return self.select(name, args, dest);
        }
        if matches!(
            short,
            "recv" | "try_recv" | "try_send" | "close" | "len" | "cap" | "clone"
        ) && self.chan_method(short, args, dest, dt)
        {
            return;
        }
        if self.mem_intrinsic(name, args, dest, dt) {
            return;
        }
        match (ty_name, short) {
            (_, "slice_range") => {
                // args: &base, start, end, flags (bit 0: has end, bit 1: inclusive).
                let bt = match self.op_ty(&args[0]) {
                    Ty::Ref(_, t) => *t,
                    t => t,
                };
                let ba = self.op_scalar(&args[0]);
                let (data, len, size, is_str) =
                    if self.cg.is_lang(&bt, "Vec") || self.cg.is_lang(&bt, "String") {
                        let size = if self.cg.is_lang(&bt, "String") {
                            1
                        } else {
                            self.cg.layout(&self.elem_of(&bt)).size
                        };
                        (
                            self.load(types::I64, ba, 0),
                            self.load(types::I64, ba, 16),
                            size,
                            self.cg.is_lang(&bt, "String"),
                        )
                    } else if let Ty::Array(et, n) = &bt {
                        let size = self.cg.layout(et).size;
                        (ba, self.iconst(*n as i64), size, false)
                    } else {
                        // `&str` / `&[T]`: the reference operand is fat.
                        let a = self.op_addr(&args[0]);
                        let is_str = matches!(bt, Ty::Prim(kiln_resolve::PrimTy::Str));
                        let size = if is_str {
                            1
                        } else {
                            self.cg.layout(&self.elem_of(&bt)).size
                        };
                        (
                            self.load(types::I64, a, 0),
                            self.load(types::I64, a, 8),
                            size,
                            is_str,
                        )
                    };
                let to64 = |cx: &mut Self, o: &Operand| {
                    let (v, t) = cx.scalar_through_refs(o);
                    match cx.cg.scalar(&t) {
                        Some(st) if st.bits() < 64 => cx.b.ins().uextend(types::I64, v),
                        _ => v,
                    }
                };
                let start = to64(self, &args[1]);
                let end_v = to64(self, &args[2]);
                let flags = to64(self, &args[3]);
                let is_str_v = self.iconst(is_str as i64);
                let end = self
                    .call_rt(
                        "kn_slice_check",
                        &[data, len, start, end_v, flags, is_str_v],
                    )
                    .unwrap();
                let off = self.b.ins().imul_imm(start, size as i64);
                let p = self.b.ins().iadd(data, off);
                let n = self.b.ins().isub(end, start);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            (_, "go_handle") => {
                let h = match self.last_task {
                    Some(s) => self.b.ins().stack_load(types::I64, s, 0),
                    None => self.iconst(0),
                };
                self.store(h, dest, 0);
            }
            ("Task", "join") => {
                // Result<T, Panic>: Ok = 0. (Panics end the program, so never Err.)
                let s = self.slot(8, 8);
                self.store_op(&args[0], s);
                let h = self.load(types::I64, s, 0);
                let (off, _) = self.cg.field(dt, "Ok.0");
                let p = self.b.ins().iadd_imm(dest, off as i64);
                self.call_rt("kn_task_join", &[h, p]);
                let z = self.b.ins().iconst(types::I32, 0);
                self.set_tag(dest, z);
            }
            (_, "scope") => {
                // scope(body): run `body(&scope)`, then wait for the scope's tasks.
                let sc = self.call_rt("kn_scope_new", &[]).unwrap();
                let fa = self.op_addr(&args[0]);
                let code = self.load(types::I64, fa, 0);
                let env = self.load(types::I64, fa, 8);
                let arg = self.slot(8, 8);
                self.store(sc, arg, 0);
                let ret = self.slot(8, 8);
                let sig = self.cg.ptr_sig(3);
                let sref = self.b.import_signature(sig);
                self.b.ins().call_indirect(sref, code, &[ret, env, arg]);
                self.call_rt("kn_scope_wait", &[sc]);
            }
            ("Scope", "spawn") => {
                let sc = self.op_scalar(&args[0]);
                let fa = self.op_addr(&args[1]);
                let code = self.load(types::I64, fa, 0);
                let env = self.load(types::I64, fa, 8);
                self.call_rt("kn_scope_spawn", &[sc, code, env]);
            }
            (_, "chan") => {
                // chan::<T>(cap) -> (chan<- T, <-chan T), both pointing at one channel.
                let et = match dt {
                    Ty::Tuple(ts) => match ts.first() {
                        Some(Ty::ChanSend(t)) => (**t).clone(),
                        _ => Ty::Unit,
                    },
                    _ => Ty::Unit,
                };
                let size = self.cg.layout(&et).size as i64;
                let size = self.iconst(size);
                let cap = match args.first() {
                    Some(a) => {
                        let (v, t) = self.scalar_through_refs(a);
                        let st = self.cg.scalar(&t).unwrap_or(types::I64);
                        if st.bits() < 64 {
                            self.b.ins().uextend(types::I64, v)
                        } else {
                            v
                        }
                    }
                    None => self.iconst(0),
                };
                let drop = self.drop_fn_ptr(&et);
                let h = self.call_rt("kn_chan_new", &[size, cap, drop]).unwrap();
                self.store(h, dest, 0);
                self.store(h, dest, 8);
            }
            (_, "chan_send") => {
                let Some((h, _)) = self.chan_handle(&args[0]) else {
                    return self.error("internal error: send on a non-channel");
                };
                let vt = self.op_ty(&args[1]);
                let s = self.slot_for(&vt);
                self.store_op(&args[1], s);
                self.call_rt("kn_chan_send", &[h, s]);
            }
            (_, "chan_recv") => {
                let Some((h, _)) = self.chan_handle(&args[0]) else {
                    return self.error("internal error: receive on a non-channel");
                };
                let ok = self.call_rt("kn_chan_recv", &[h, dest]).unwrap();
                let closed = self.b.ins().icmp_imm(IntCC::Equal, ok, 0);
                self.when(closed, |cx| cx.panic_msg("receive on a closed channel"));
            }
            (_, "select_sent") => {
                // The runtime already moved the value into the channel.
                if let Some(p) = args[0].place() {
                    self.moved(p);
                }
            }
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
