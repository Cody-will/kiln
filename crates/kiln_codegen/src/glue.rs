//! Drop glue, clone glue, and trait-object vtables.
//!
//! Drop glue for a type is a function `fn(*mut T)` that runs the value's
//! destructor: the user's `Drop::drop` (if any), then the fields' glue; for
//! `String`/`Vec<T>` it drops the elements and frees the buffer; for `Box<T>`
//! it drops the pointee and frees it (for `Box<dyn Trait>` through the vtable).
//!
//! Clone glue is `fn(src: *const T, dst: *mut T)` and implements `.clone()`
//! for `String`, `Vec<T>`, `Box<T>`, tuples, and types that derive `Clone`.
//!
//! A vtable is `[drop_glue, size, align, method0, method1, ...]` with the
//! trait's methods in declaration order.

use crate::func::{flags, FnCx, Target};
use crate::{Abi, Codegen, Work};
use cranelift_codegen::ir::{condcodes::IntCC, types, InstBuilder, Value};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use kiln_resolve::{DefId, DefKind};
use kiln_typeck::ty::Ty;
use std::collections::HashMap;

impl<'r, 'a> Codegen<'r, 'a> {
    pub(crate) fn drop_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.drop_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(1);
        self.counter += 1;
        let name = format!("kn_drop{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.drop_glue.insert(key, id);
        self.queue.push_back(Work::DropGlue { id, ty: t.clone() });
        id
    }

    /// Runs `f` with a translation context for a glue function with `n` pointer parameters.
    fn with_glue(
        &mut self,
        id: FuncId,
        n: usize,
        f: impl FnOnce(&mut FnCx<'_, 'r, 'a, '_>, &[Value]),
    ) {
        let mut ctx = self.module.make_context();
        ctx.func.signature = self.ptr_sig(n);
        let mut fbc = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let e = b.create_block();
            b.append_block_params_for_function_params(e);
            b.switch_to_block(e);
            let ps = b.block_params(e).to_vec();
            let span = self.span;
            let body = kiln_mir::Body {
                name: String::new(),
                kind: kiln_mir::BodyKind::Fn,
                locals: vec![],
                blocks: vec![],
                arg_count: 0,
                return_flows: None,
                span,
                errors: vec![],
                unsupported: vec![],
                captures: vec![],
            };
            let mut cx = FnCx {
                cg: self,
                b,
                body,
                substs: HashMap::new(),
                fn_idx: 0,
                locals: vec![],
                blocks: vec![],
                defer_stack: None,
                last_task: None,
                intr_substs: HashMap::new(),
                cur_block: 0,
                preempt: None,
                span,
                promo: Default::default(),
            };
            f(&mut cx, &ps);
            cx.b.ins().return_(&[]);
            cx.b.seal_all_blocks();
            cx.b.finalize();
        }
        if let Err(e) = self.module.define_function(id, &mut ctx) {
            self.fatal(format!("internal code generation error in glue: {e:?}"));
        }
    }

    pub(crate) fn variants_of(&self, t: &Ty) -> Vec<Option<DefId>> {
        match t {
            Ty::Adt(d, _) if self.r.def(*d).kind == DefKind::Enum => {
                self.r.variants(*d).into_iter().map(Some).collect()
            }
            _ => vec![None],
        }
    }

    /// The out-of-line equality function for `t` (handles recursive types).
    pub(crate) fn eq_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.eq_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(3);
        self.counter += 1;
        let name = format!("kn_eq{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.eq_glue.insert(key, id);
        self.queue.push_back(Work::EqGlue { id, ty: t.clone() });
        id
    }

    /// The out-of-line structural clone for ADT `t` (handles recursive types).
    pub(crate) fn clone_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.clone_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(2);
        self.counter += 1;
        let name = format!("kn_clone{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.clone_glue.insert(key, id);
        self.queue.push_back(Work::CloneGlue { id, ty: t.clone() });
        id
    }

    pub(crate) fn define_clone_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 2, |cx, ps| cx.clone_fields(ps[1], ps[0], &t));
    }

    pub(crate) fn define_eq_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 3, |cx, ps| {
            let one = cx.b.ins().iconst(types::I8, 1);
            cx.store(one, ps[0], 0);
            cx.eq_fields(ps[1], ps[2], &t, ps[0]);
        });
    }

    pub(crate) fn define_drop_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 1, |cx, ps| {
            let p = ps[0];
            cx.drop_in_place(p, &t);
        });
    }

    /// The vtable for `concrete` as `dyn trait_`.
    pub(crate) fn vtable(&mut self, concrete: &Ty, trait_: Option<DefId>) -> DataId {
        let key = format!("{concrete:?}/{trait_:?}");
        if let Some(id) = self.vtables.get(&key) {
            return *id;
        }
        self.counter += 1;
        let name = format!("kn_vtable{}", self.counter);
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("declare vtable");
        self.vtables.insert(key, id);
        let l = self.layout(concrete);
        let methods = trait_
            .map(|t| self.typed.dyn_methods(t))
            .unwrap_or_default();
        let mut d = DataDescription::new();
        let mut bytes = vec![0u8; 24 + 8 * methods.len()];
        bytes[8..16].copy_from_slice(&(l.size as u64).to_le_bytes());
        bytes[16..24].copy_from_slice(&(l.align as u64).to_le_bytes());
        d.define(bytes.into_boxed_slice());
        let drop = self.drop_glue_for(concrete);
        let fr = self.module.declare_func_in_data(drop, &mut d);
        d.write_function_addr(0, fr);
        for (k, m) in methods.iter().enumerate() {
            let mut substs = HashMap::new();
            substs.insert("Self".to_string(), concrete.clone());
            match self.typed.resolve_trait_method(*m, &substs) {
                Some((i, s)) => {
                    let f = self.instance(i, 0, s, Abi::Plain);
                    let fr = self.module.declare_func_in_data(f, &mut d);
                    d.write_function_addr(24 + 8 * k as u32, fr);
                }
                // Generic methods can't be called through `dyn` (typeck rejects
                // those calls), so their slots stay empty; anything else is a bug.
                None if !self.typed.fn_generics(*m).0.is_empty() => {}
                None => {
                    let name = self.r.fns[*m].decl.name.name.clone();
                    self.fatal(format!(
                        "no implementation of `{name}` in the vtable for {concrete:?}"
                    ));
                }
            }
        }
        if let Err(e) = self.module.define_data(id, &d) {
            self.fatal(format!("internal error defining vtable: {e}"));
        }
        id
    }
}

impl FnCx<'_, '_, '_, '_> {
    /// Emits the destructor of the value of type `t` at `p`.
    pub(crate) fn drop_in_place(&mut self, p: Value, t: &Ty) {
        if !self.cg.needs_drop(t) {
            return;
        }
        if self.cg.is_lang(t, "string") {
            let one = self.iconst(1);
            self.call_rt("kn_vec_free", &[p, one, one]);
            return;
        }
        if let Ty::Adt(_, a) = t {
            if self.cg.is_lang(t, "Vec") {
                let et = a[0].clone();
                let l = self.cg.layout(&et);
                if self.cg.needs_drop(&et) {
                    self.for_each_elem(p, l.size, |cx, e| cx.drop_value(e, &et));
                }
                let (s, al) = (self.iconst(l.size as i64), self.iconst(l.align as i64));
                self.call_rt("kn_vec_free", &[p, s, al]);
                return;
            }
            if self.cg.is_lang(t, "Box") {
                let inner = a[0].clone();
                let data = self.load(types::I64, p, 0);
                if let Ty::Dyn(_) = inner {
                    let vt = self.load(types::I64, p, 8);
                    let dropf = self.load(types::I64, vt, 0);
                    let sig = self.cg.ptr_sig(1);
                    let sref = self.b.import_signature(sig);
                    self.b.ins().call_indirect(sref, dropf, &[data]);
                    let size = self.load(types::I64, vt, 8);
                    let align = self.load(types::I64, vt, 16);
                    self.call_rt("kn_free", &[data, size, align]);
                } else {
                    self.drop_value(data, &inner);
                    let l = self.cg.layout(&inner);
                    let (s, al) = (self.iconst(l.size as i64), self.iconst(l.align as i64));
                    self.call_rt("kn_free", &[data, s, al]);
                }
                return;
            }
        }
        if let Ty::ChanSend(_) | Ty::ChanRecv(_) = t {
            let h = self.load(types::I64, p, 0);
            let s = self
                .b
                .ins()
                .iconst(types::I32, matches!(t, Ty::ChanSend(_)) as i64);
            self.call_rt("kn_chan_drop_end", &[h, s]);
            return;
        }
        if self.cg.is_lang(t, "Task") {
            let h = self.load(types::I64, p, 0);
            self.call_rt("kn_task_drop", &[h]);
            return;
        }
        // User `Drop` impl first.
        if matches!(t, Ty::Adt(..)) && self.cg.typed.has_drop_impl(t) {
            if let Some(dt) = self.cg.r.prelude_type("Drop") {
                if let Some(&m) = self.cg.typed.trait_methods(dt).first() {
                    let mut substs = HashMap::new();
                    substs.insert("Self".to_string(), t.clone());
                    if let Target::Direct(id) = self.resolve(m, substs) {
                        let r = self.slot(8, 8);
                        self.store(p, r, 0);
                        let ret = self.slot(1, 1);
                        self.call_fn(id, &[ret, r]);
                    }
                }
            }
        }
        match t {
            Ty::Array(et, n) => {
                let size = self.cg.layout(et).size;
                let et = (**et).clone();
                self.for_each_index(p, *n, size, |cx, e| cx.drop_value(e, &et));
            }
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let a = self.b.ins().iadd_imm(p, off as i64);
                    self.drop_value(a, et);
                }
            }
            Ty::Adt(..) => {
                let variants = self.cg.variants_of(t);
                let is_enum = variants.first().is_some_and(|v| v.is_some());
                for (vi, v) in variants.into_iter().enumerate() {
                    let fields = self.cg.typed.adt_fields(t, v);
                    if !fields.iter().any(|(_, ft)| self.cg.needs_drop(ft)) {
                        continue;
                    }
                    let prefix = v
                        .map(|v| format!("{}.", self.cg.r.def(v).name))
                        .unwrap_or_default();
                    let body = |cx: &mut Self| {
                        for (n, ft) in &fields {
                            let (off, _) = cx.cg.field(t, &format!("{prefix}{n}"));
                            let a = cx.b.ins().iadd_imm(p, off as i64);
                            cx.drop_value(a, ft);
                        }
                    };
                    if is_enum {
                        let tag = self.load(types::I32, p, 0);
                        let is = self.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
                        self.when(is, body);
                    } else {
                        body(self);
                    }
                }
            }
            _ => {}
        }
    }

    /// Loops over the `n` elements (each `size` bytes) of the array at `base`.
    pub(crate) fn for_each_index(
        &mut self,
        base: Value,
        n: u64,
        size: u32,
        f: impl Fn(&mut Self, Value),
    ) {
        if n <= 4 {
            for i in 0..n {
                let e = self.b.ins().iadd_imm(base, (i * size as u64) as i64);
                f(self, e);
            }
            return;
        }
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
        let more = self.b.ins().icmp_imm(IntCC::UnsignedLessThan, iv, n as i64);
        self.b.ins().brif(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        let off = self.b.ins().imul_imm(iv, size as i64);
        let e = self.b.ins().iadd(base, off);
        f(self, e);
        let next = self.b.ins().iadd_imm(iv, 1);
        self.store(next, i, 0);
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(done);
    }

    /// Loops over the elements of the `Vec` at `v`.
    fn for_each_elem(&mut self, v: Value, size: u32, f: impl Fn(&mut Self, Value)) {
        let data = self.load(types::I64, v, 0);
        let len = self.load(types::I64, v, 16);
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
        f(self, e);
        let next = self.b.ins().iadd_imm(iv, 1);
        self.store(next, i, 0);
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(done);
    }

    /// Calls a hand-written `impl Clone` for `t`, if there is one.
    fn user_clone(&mut self, src: Value, dst: Value, t: &Ty) -> bool {
        let r = self.cg.r;
        let Some(cl) = r.prelude_type("Clone") else {
            return false;
        };
        let Some(tf) = (0..r.fns.len())
            .find(|&i| r.fns[i].trait_def == Some(cl) && r.fns[i].decl.name.name == "clone")
        else {
            return false;
        };
        let mut substs = HashMap::new();
        substs.insert("Self".to_string(), t.clone());
        match self.cg.typed.resolve_trait_method(tf, &substs) {
            Some((i2, s2)) if i2 != tf => {
                let id = self.cg.instance(i2, 0, s2, Abi::Plain);
                let arg = self.slot(8, 8);
                self.store(src, arg, 0);
                self.call_fn(id, &[dst, arg]);
                true
            }
            _ => false,
        }
    }

    /// Whether the values at `a` and `b` (type `t`) are equal (an `i8` 0/1).
    /// Derived `Eq` is structural: fields, enum variants and payloads,
    /// tuples, arrays, strings, vectors, and boxes, looking through references.
    pub(crate) fn eq_at(&mut self, a: Value, b: Value, t: &Ty) -> Value {
        let res = self.slot(1, 1);
        let one = self.b.ins().iconst(types::I8, 1);
        self.store(one, res, 0);
        self.eq_into(a, b, t, res);
        self.load(types::I8, res, 0)
    }

    /// Clears `*res` unless the values at `a` and `b` are equal.
    fn eq_into(&mut self, a: Value, b: Value, t: &Ty, res: Value) {
        let and = |cx: &mut Self, eq: Value| {
            let r = cx.load(types::I8, res, 0);
            let r = cx.b.ins().band(r, eq);
            cx.store(r, res, 0);
        };
        if self.cg.is_lang(t, "string") || matches!(t, Ty::Prim(kiln_resolve::PrimTy::Str)) {
            let (p, n) = if matches!(t, Ty::Prim(_)) {
                return self.error("cannot compare unsized `str` values");
            } else {
                (self.load(types::I64, a, 0), self.load(types::I64, a, 16))
            };
            let (q, m) = (self.load(types::I64, b, 0), self.load(types::I64, b, 16));
            let e = self.call_rt("kn_str_eq", &[p, n, q, m]).unwrap();
            let e = self.b.ins().icmp_imm(IntCC::NotEqual, e, 0);
            return and(self, e);
        }
        match t {
            Ty::Unit | Ty::Never => {}
            Ty::Ref(_, inner) | Ty::Ptr(_, inner)
                if matches!(**inner, Ty::Prim(kiln_resolve::PrimTy::Str)) =>
            {
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                let (q, m) = (self.load(types::I64, b, 0), self.load(types::I64, b, 8));
                let e = self.call_rt("kn_str_eq", &[p, n, q, m]).unwrap();
                let e = self.b.ins().icmp_imm(IntCC::NotEqual, e, 0);
                and(self, e);
            }
            Ty::Ref(_, inner) if !self.cg.is_fat(t) => {
                let (pa, pb) = (self.load(types::I64, a, 0), self.load(types::I64, b, 0));
                let inner = (**inner).clone();
                self.eq_into(pa, pb, &inner, res);
            }
            Ty::Prim(kiln_resolve::PrimTy::F32 | kiln_resolve::PrimTy::F64) => {
                let st = self.cg.scalar(t).unwrap();
                let (x, y) = (self.load(st, a, 0), self.load(st, b, 0));
                let e = self
                    .b
                    .ins()
                    .fcmp(cranelift_codegen::ir::condcodes::FloatCC::Equal, x, y);
                and(self, e);
            }
            _ if self.cg.scalar(t).is_some() && !self.cg.is_lang(t, "Box") => {
                let st = self.cg.scalar(t).unwrap();
                let (x, y) = (self.load(st, a, 0), self.load(st, b, 0));
                let e = self.b.ins().icmp(IntCC::Equal, x, y);
                and(self, e);
            }
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let x = self.b.ins().iadd_imm(a, off as i64);
                    let y = self.b.ins().iadd_imm(b, off as i64);
                    self.eq_into(x, y, et, res);
                }
            }
            Ty::Array(et, n) => {
                let size = self.cg.layout(et).size;
                let et = (**et).clone();
                self.for_each_index(a, *n, size, |cx, x| {
                    let off = cx.b.ins().isub(x, a);
                    let y = cx.b.ins().iadd(b, off);
                    cx.eq_into(x, y, &et, res);
                });
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Box") => {
                let (pa, pb) = (self.load(types::I64, a, 0), self.load(types::I64, b, 0));
                let inner = args[0].clone();
                self.eq_into(pa, pb, &inner, res);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Vec") => {
                let et = args[0].clone();
                let size = self.cg.layout(&et).size;
                let (la, lb) = (self.load(types::I64, a, 16), self.load(types::I64, b, 16));
                let same = self.b.ins().icmp(IntCC::Equal, la, lb);
                and(self, same);
                let db = self.load(types::I64, b, 0);
                let da = self.load(types::I64, a, 0);
                self.when(same, |cx| {
                    cx.for_each_elem(a, size, |cx, x| {
                        let off = cx.b.ins().isub(x, da);
                        let y = cx.b.ins().iadd(db, off);
                        cx.eq_into(x, y, &et, res);
                    });
                });
            }
            Ty::Adt(..) => {
                if let Some(e) = self.user_eq(a, b, t) {
                    return and(self, e);
                }
                // Out of line, so recursive types terminate.
                let id = self.cg.eq_glue_for(t);
                let out = self.slot(1, 1);
                self.call_fn(id, &[out, a, b]);
                let e = self.load(types::I8, out, 0);
                and(self, e);
            }
            _ => {
                let n = self.cg.typed.fmt_ty(t);
                self.error(format!("cannot compare values of type `{n}`"));
            }
        }
    }

    /// Field-by-field equality of an ADT value (the body of its eq glue).
    fn eq_fields(&mut self, a: Value, b: Value, t: &Ty, res: Value) {
        let variants = self.cg.variants_of(t);
        let is_enum = variants.first().is_some_and(|v| v.is_some());
        if !is_enum {
            for (n, ft) in self.cg.typed.adt_fields(t, None) {
                let (off, _) = self.cg.field(t, &n);
                let x = self.b.ins().iadd_imm(a, off as i64);
                let y = self.b.ins().iadd_imm(b, off as i64);
                self.eq_into(x, y, &ft, res);
            }
            return;
        }
        let ta = self.load(types::I32, a, 0);
        let tb = self.load(types::I32, b, 0);
        let same = self.b.ins().icmp(IntCC::Equal, ta, tb);
        let r = self.load(types::I8, res, 0);
        let r = self.b.ins().band(r, same);
        self.store(r, res, 0);
        self.when(same, |cx| {
            for (vi, v) in variants.into_iter().enumerate() {
                let fields = cx.cg.typed.adt_fields(t, v);
                if fields.is_empty() {
                    continue;
                }
                let prefix = v
                    .map(|v| format!("{}.", cx.cg.r.def(v).name))
                    .unwrap_or_default();
                let tag = cx.load(types::I32, a, 0);
                let is = cx.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
                cx.when(is, |cx| {
                    for (n, ft) in &fields {
                        let (off, _) = cx.cg.field(t, &format!("{prefix}{n}"));
                        let x = cx.b.ins().iadd_imm(a, off as i64);
                        let y = cx.b.ins().iadd_imm(b, off as i64);
                        cx.eq_into(x, y, ft, res);
                    }
                });
            }
        });
    }

    /// Writes a clone of the value at `src` (type `t`) to `dst`.
    pub(crate) fn clone_into(&mut self, src: Value, dst: Value, t: &Ty) {
        if !self.cg.needs_drop(t) {
            let l = self.cg.layout(t);
            self.copy(dst, src, l.size);
            return;
        }
        if self.cg.is_lang(t, "string") {
            let p = self.load(types::I64, src, 0);
            let n = self.load(types::I64, src, 16);
            self.call_rt("kn_string_from", &[dst, p, n]);
            return;
        }
        if let Ty::Adt(_, a) = t {
            if self.cg.is_lang(t, "Vec") {
                let et = a[0].clone();
                let l = self.cg.layout(&et);
                let one = self.iconst(1);
                let z = self.iconst(0);
                self.store(one, dst, 0);
                self.store(z, dst, 8);
                self.store(z, dst, 16);
                let len = self.load(types::I64, src, 16);
                let (s, al) = (self.iconst(l.size as i64), self.iconst(l.align as i64));
                self.call_rt("kn_vec_reserve", &[dst, len, s, al]);
                let ddata = self.load(types::I64, dst, 0);
                let sdata = self.load(types::I64, src, 0);
                let et2 = et.clone();
                self.for_each_elem(src, l.size, move |cx, e| {
                    let off = cx.b.ins().isub(e, sdata);
                    let d = cx.b.ins().iadd(ddata, off);
                    cx.clone_into(e, d, &et2);
                });
                self.store(len, dst, 16);
                return;
            }
            if self.cg.is_lang(t, "Box") {
                let inner = a[0].clone();
                if let Ty::Dyn(_) = inner {
                    return self.error("cloning a `Box<dyn Trait>` is not supported");
                }
                let l = self.cg.layout(&inner);
                let (s, al) = (self.iconst(l.size as i64), self.iconst(l.align as i64));
                let p = self.call_rt("kn_alloc", &[s, al]).unwrap();
                let from = self.load(types::I64, src, 0);
                self.clone_into(from, p, &inner);
                self.store(p, dst, 0);
                return;
            }
        }
        if let Ty::ChanSend(_) | Ty::ChanRecv(_) = t {
            let h = self.load(types::I64, src, 0);
            let s = self
                .b
                .ins()
                .iconst(types::I32, matches!(t, Ty::ChanSend(_)) as i64);
            self.call_rt("kn_chan_clone_end", &[h, s]);
            self.store(h, dst, 0);
            return;
        }
        match t {
            Ty::Array(et, n) => {
                let size = self.cg.layout(et).size;
                let et = (**et).clone();
                self.for_each_index(dst, *n, size, |cx, d| {
                    let off = cx.b.ins().isub(d, dst);
                    let s = cx.b.ins().iadd(src, off);
                    cx.clone_into(s, d, &et);
                });
            }
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let s = self.b.ins().iadd_imm(src, off as i64);
                    let d = self.b.ins().iadd_imm(dst, off as i64);
                    self.clone_into(s, d, et);
                }
            }
            Ty::Adt(..) if self.user_clone(src, dst, t) => {}
            Ty::Adt(..) => {
                // Out of line, so recursive types terminate.
                let id = self.cg.clone_glue_for(t);
                self.call_fn(id, &[dst, src]);
            }
            _ => {
                let l = self.cg.layout(t);
                self.copy(dst, src, l.size);
            }
        }
        let _ = flags();
    }

    /// Field-by-field clone of an ADT value (the body of its clone glue).
    fn clone_fields(&mut self, src: Value, dst: Value, t: &Ty) {
        {
            {
                let variants = self.cg.variants_of(t);
                let is_enum = variants.first().is_some_and(|v| v.is_some());
                if is_enum {
                    let tag = self.load(types::I32, src, 0);
                    self.store(tag, dst, 0);
                }
                for (vi, v) in variants.into_iter().enumerate() {
                    let fields = self.cg.typed.adt_fields(t, v);
                    let prefix = v
                        .map(|v| format!("{}.", self.cg.r.def(v).name))
                        .unwrap_or_default();
                    let body = |cx: &mut Self| {
                        for (n, ft) in &fields {
                            let (off, _) = cx.cg.field(t, &format!("{prefix}{n}"));
                            let s = cx.b.ins().iadd_imm(src, off as i64);
                            let d = cx.b.ins().iadd_imm(dst, off as i64);
                            cx.clone_into(s, d, ft);
                        }
                    };
                    if is_enum {
                        let tag = self.load(types::I32, src, 0);
                        let is = self.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
                        self.when(is, body);
                    } else {
                        body(self);
                    }
                }
            }
        }
    }
}

/// Ordering and hashing glue: the structural `Ord::cmp` and `Hash::hash`
/// behind derived (and built-in) impls, and lookups of hand-written impls,
/// which take precedence wherever a value of that type is nested.
impl Codegen<'_, '_> {
    /// The out-of-line ordering function for `t`: `(out: *mut i8, a, b)`.
    pub(crate) fn cmp_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.cmp_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(3);
        self.counter += 1;
        let name = format!("kn_cmp{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.cmp_glue.insert(key, id);
        self.queue.push_back(Work::CmpGlue { id, ty: t.clone() });
        id
    }

    /// The out-of-line hash function for `t`: `(hasher: *mut u64, value)`.
    pub(crate) fn hash_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.hash_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(2);
        self.counter += 1;
        let name = format!("kn_hash{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.hash_glue.insert(key, id);
        self.queue.push_back(Work::HashGlue { id, ty: t.clone() });
        id
    }

    pub(crate) fn define_cmp_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 3, |cx, ps| {
            let z = cx.b.ins().iconst(types::I8, 0);
            cx.store(z, ps[0], 0);
            cx.cmp_fields(ps[1], ps[2], &t, ps[0]);
        });
    }

    pub(crate) fn define_hash_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 2, |cx, ps| cx.hash_fields(ps[1], &t, ps[0]));
    }

    /// The hand-written implementation of `trait_name::method` for `t`, if
    /// `t` has one (rather than using the trait's default method).
    pub(crate) fn user_impl(&mut self, t: &Ty, trait_name: &str, method: &str) -> Option<FuncId> {
        let Ty::Adt(d, _) = t else { return None };
        if ["string", "Vec", "Box"].iter().any(|n| self.is_lang(t, n)) || self.r.is_core(*d) {
            return None;
        }
        let r = self.r;
        let tr = r.prelude_type(trait_name)?;
        let tf = (0..r.fns.len())
            .find(|&i| r.fns[i].trait_def == Some(tr) && r.fns[i].decl.name.name == method)?;
        let mut substs = HashMap::new();
        substs.insert("Self".to_string(), t.clone());
        match self.typed.resolve_trait_method(tf, &substs) {
            Some((i2, s2)) if i2 != tf => Some(self.instance(i2, 0, s2, Abi::Plain)),
            _ => None,
        }
    }
}

impl FnCx<'_, '_, '_, '_> {
    /// Compares the values at `a` and `b` (type `t`): an `i8` -1, 0, or 1.
    pub(crate) fn cmp_at(&mut self, a: Value, b: Value, t: &Ty) -> Value {
        let res = self.slot(1, 1);
        let z = self.b.ins().iconst(types::I8, 0);
        self.store(z, res, 0);
        self.cmp_into(a, b, t, res);
        self.load(types::I8, res, 0)
    }

    /// The structural ordering of an ADT, ignoring its own `Ord` impl (but
    /// not those of its fields): the body of the default `Ord::cmp`.
    pub(crate) fn derived_cmp(&mut self, a: Value, b: Value, t: &Ty) -> Value {
        if !matches!(t, Ty::Adt(..))
            || self.cg.is_lang(t, "Vec")
            || self.cg.is_lang(t, "string")
            || self.cg.is_lang(t, "Box")
        {
            return self.cmp_at(a, b, t);
        }
        let id = self.cg.cmp_glue_for(t);
        let out = self.slot(1, 1);
        self.call_fn(id, &[out, a, b]);
        self.load(types::I8, out, 0)
    }

    /// Structural equality of an ADT ignoring its own `Eq` impl.
    pub(crate) fn derived_eq(&mut self, a: Value, b: Value, t: &Ty) -> Value {
        if !matches!(t, Ty::Adt(..))
            || self.cg.is_lang(t, "Vec")
            || self.cg.is_lang(t, "string")
            || self.cg.is_lang(t, "Box")
        {
            return self.eq_at(a, b, t);
        }
        let id = self.cg.eq_glue_for(t);
        let out = self.slot(1, 1);
        self.call_fn(id, &[out, a, b]);
        self.load(types::I8, out, 0)
    }

    /// Structural hash of a value ignoring its own `Hash` impl.
    pub(crate) fn derived_hash(&mut self, a: Value, t: &Ty, h: Value) {
        if !matches!(t, Ty::Adt(..))
            || self.cg.is_lang(t, "Vec")
            || self.cg.is_lang(t, "string")
            || self.cg.is_lang(t, "Box")
        {
            return self.hash_into(a, t, h);
        }
        let id = self.cg.hash_glue_for(t);
        self.call_fn(id, &[h, a]);
    }

    /// Calls a hand-written `eq` for `t`; returns its `i8` result.
    pub(crate) fn user_eq(&mut self, a: Value, b: Value, t: &Ty) -> Option<Value> {
        let id = self.cg.user_impl(t, "Eq", "eq")?;
        Some(self.call_ref_method(id, a, b, 1))
    }

    /// Calls `id(ret, &a, &b)` and loads the `size`-byte scalar result.
    fn call_ref_method(&mut self, id: FuncId, a: Value, b: Value, size: u32) -> Value {
        let out = self.slot(size.max(1), size.max(1));
        let sa = self.slot(8, 8);
        self.store(a, sa, 0);
        let sb = self.slot(8, 8);
        self.store(b, sb, 0);
        self.call_fn(id, &[out, sa, sb]);
        let st = if size == 4 { types::I32 } else { types::I8 };
        self.load(st, out, 0)
    }

    /// Stores `gt - lt` (each an `i8` 0/1) into `*res`.
    fn set_order(&mut self, lt: Value, gt: Value, res: Value) {
        let v = self.b.ins().isub(gt, lt);
        self.store(v, res, 0);
    }

    /// Compares `a` and `b` into `*res` unless an earlier part already decided it.
    fn cmp_into(&mut self, a: Value, b: Value, t: &Ty, res: Value) {
        let r = self.load(types::I8, res, 0);
        let undecided = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
        let t = t.clone();
        self.when(undecided, |cx| cx.cmp_now(a, b, &t, res));
    }

    fn cmp_strs(&mut self, (p, n): (Value, Value), (q, m): (Value, Value), res: Value) {
        let c = self.call_rt("kn_str_cmp", &[p, n, q, m]).unwrap();
        let lt = self.b.ins().icmp_imm(IntCC::SignedLessThan, c, 0);
        let gt = self.b.ins().icmp_imm(IntCC::SignedGreaterThan, c, 0);
        self.set_order(lt, gt, res);
    }

    /// Lexicographic comparison of two sequences (`data`, `len`).
    fn cmp_seq(&mut self, (da, la): (Value, Value), (db, lb): (Value, Value), et: &Ty, res: Value) {
        let size = self.cg.layout(et).size;
        let shorter = self.b.ins().icmp(IntCC::UnsignedLessThan, la, lb);
        let n = self.b.ins().select(shorter, la, lb);
        let et2 = et.clone();
        self.for_count(n, |cx, i| {
            let off = cx.b.ins().imul_imm(i, size as i64);
            let x = cx.b.ins().iadd(da, off);
            let y = cx.b.ins().iadd(db, off);
            cx.cmp_into(x, y, &et2, res);
        });
        let r = self.load(types::I8, res, 0);
        let undecided = self.b.ins().icmp_imm(IntCC::Equal, r, 0);
        self.when(undecided, |cx| {
            let lt = cx.b.ins().icmp(IntCC::UnsignedLessThan, la, lb);
            let gt = cx.b.ins().icmp(IntCC::UnsignedGreaterThan, la, lb);
            cx.set_order(lt, gt, res);
        });
    }

    /// Runs `f(i)` for `i` in `0..n`.
    fn for_count(&mut self, n: Value, f: impl Fn(&mut Self, Value)) {
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
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, iv, n);
        self.b.ins().brif(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        f(self, iv);
        let next = self.b.ins().iadd_imm(iv, 1);
        self.store(next, i, 0);
        self.b.ins().jump(head, &[]);
        self.b.switch_to_block(done);
    }

    /// Compares assuming `*res` is 0 (undecided so far).
    fn cmp_now(&mut self, a: Value, b: Value, t: &Ty, res: Value) {
        use kiln_resolve::PrimTy;
        if self.cg.is_lang(t, "string") {
            let x = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
            let y = (self.load(types::I64, b, 0), self.load(types::I64, b, 16));
            return self.cmp_strs(x, y, res);
        }
        match t {
            Ty::Unit | Ty::Never => {}
            Ty::Ref(_, inner) | Ty::Ptr(_, inner) if matches!(**inner, Ty::Prim(PrimTy::Str)) => {
                let x = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                let y = (self.load(types::I64, b, 0), self.load(types::I64, b, 8));
                self.cmp_strs(x, y, res);
            }
            Ty::Ref(_, inner) if matches!(**inner, Ty::Slice(_)) => {
                let Ty::Slice(et) = &**inner else {
                    unreachable!()
                };
                let x = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                let y = (self.load(types::I64, b, 0), self.load(types::I64, b, 8));
                let et = (**et).clone();
                self.cmp_seq(x, y, &et, res);
            }
            Ty::Ref(_, inner) if !self.cg.is_fat(t) => {
                let (pa, pb) = (self.load(types::I64, a, 0), self.load(types::I64, b, 0));
                let inner = (**inner).clone();
                self.cmp_now(pa, pb, &inner, res);
            }
            Ty::Prim(PrimTy::F32 | PrimTy::F64) => {
                use cranelift_codegen::ir::condcodes::FloatCC;
                let st = self.cg.scalar(t).unwrap();
                let (x, y) = (self.load(st, a, 0), self.load(st, b, 0));
                let lt = self.b.ins().fcmp(FloatCC::LessThan, x, y);
                let gt = self.b.ins().fcmp(FloatCC::GreaterThan, x, y);
                self.set_order(lt, gt, res);
            }
            _ if self.cg.scalar(t).is_some()
                && !self.cg.is_lang(t, "Box")
                && !matches!(t, Ty::Adt(..)) =>
            {
                let st = self.cg.scalar(t).unwrap();
                let (x, y) = (self.load(st, a, 0), self.load(st, b, 0));
                let (l, g) = if Self::is_signed(t) {
                    (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
                } else {
                    (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
                };
                let lt = self.b.ins().icmp(l, x, y);
                let gt = self.b.ins().icmp(g, x, y);
                self.set_order(lt, gt, res);
            }
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let x = self.b.ins().iadd_imm(a, off as i64);
                    let y = self.b.ins().iadd_imm(b, off as i64);
                    self.cmp_into(x, y, et, res);
                }
            }
            Ty::Array(et, n) => {
                let n = self.iconst(*n as i64);
                let et = (**et).clone();
                self.cmp_seq((a, n), (b, n), &et, res);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Box") => {
                let (pa, pb) = (self.load(types::I64, a, 0), self.load(types::I64, b, 0));
                let inner = args[0].clone();
                self.cmp_now(pa, pb, &inner, res);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Vec") => {
                let x = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
                let y = (self.load(types::I64, b, 0), self.load(types::I64, b, 16));
                let et = args[0].clone();
                self.cmp_seq(x, y, &et, res);
            }
            Ty::Adt(..) => {
                if let Some(id) = self.cg.user_impl(t, "Ord", "cmp") {
                    // `Ordering` is Less = 0, Equal = 1, Greater = 2.
                    let tag = self.call_ref_method(id, a, b, 4);
                    let v = self.b.ins().iadd_imm(tag, -1);
                    let v = self.b.ins().ireduce(types::I8, v);
                    self.store(v, res, 0);
                    return;
                }
                let v = self.derived_cmp(a, b, t);
                self.store(v, res, 0);
            }
            _ => {
                let n = self.cg.typed.fmt_ty(t);
                self.error(format!("cannot order values of type `{n}`"));
            }
        }
    }

    /// Field-by-field ordering of an ADT (the body of its cmp glue). Enum
    /// values order by variant (declaration order), then by payload.
    fn cmp_fields(&mut self, a: Value, b: Value, t: &Ty, res: Value) {
        let variants = self.cg.variants_of(t);
        let is_enum = variants.first().is_some_and(|v| v.is_some());
        if !is_enum {
            for (n, ft) in self.cg.typed.adt_fields(t, None) {
                let (off, _) = self.cg.field(t, &n);
                let x = self.b.ins().iadd_imm(a, off as i64);
                let y = self.b.ins().iadd_imm(b, off as i64);
                self.cmp_into(x, y, &ft, res);
            }
            return;
        }
        let ta = self.load(types::I32, a, 0);
        let tb = self.load(types::I32, b, 0);
        let lt = self.b.ins().icmp(IntCC::UnsignedLessThan, ta, tb);
        let gt = self.b.ins().icmp(IntCC::UnsignedGreaterThan, ta, tb);
        // `None` orders before `Some` although `Some` is declared first.
        let is_option = matches!(t, Ty::Adt(d, _) if self.cg.r.is_core(*d) && self.cg.r.def(*d).name == "option");
        let (lt, gt) = if is_option { (gt, lt) } else { (lt, gt) };
        self.set_order(lt, gt, res);
        let same = self.b.ins().icmp(IntCC::Equal, ta, tb);
        let t = t.clone();
        self.when(same, |cx| {
            for (vi, v) in variants.into_iter().enumerate() {
                let fields = cx.cg.typed.adt_fields(&t, v);
                if fields.is_empty() {
                    continue;
                }
                let prefix = v
                    .map(|v| format!("{}.", cx.cg.r.def(v).name))
                    .unwrap_or_default();
                let tag = cx.load(types::I32, a, 0);
                let is = cx.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
                cx.when(is, |cx| {
                    for (n, ft) in &fields {
                        let (off, _) = cx.cg.field(&t, &format!("{prefix}{n}"));
                        let x = cx.b.ins().iadd_imm(a, off as i64);
                        let y = cx.b.ins().iadd_imm(b, off as i64);
                        cx.cmp_into(x, y, ft, res);
                    }
                });
            }
        });
    }

    fn hash_seq(&mut self, data: Value, len: Value, et: &Ty, h: Value) {
        self.call_rt("kn_hash_u64", &[h, len]);
        let size = self.cg.layout(et).size;
        let et2 = et.clone();
        self.for_count(len, |cx, i| {
            let off = cx.b.ins().imul_imm(i, size as i64);
            let x = cx.b.ins().iadd(data, off);
            cx.hash_into(x, &et2, h);
        });
    }

    /// Feeds the value at `a` (type `t`) to the hasher state at `h`. Values
    /// that are equal hash the same; `-0.0` and `0.0` are not distinguished.
    pub(crate) fn hash_into(&mut self, a: Value, t: &Ty, h: Value) {
        use kiln_resolve::PrimTy;
        if self.cg.is_lang(t, "string") {
            let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
            self.call_rt("kn_hash_bytes", &[h, p, n]);
            return;
        }
        match t {
            Ty::Unit | Ty::Never => {}
            Ty::Ref(_, inner) | Ty::Ptr(_, inner) if matches!(**inner, Ty::Prim(PrimTy::Str)) => {
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                self.call_rt("kn_hash_bytes", &[h, p, n]);
            }
            Ty::Ref(_, inner) if matches!(**inner, Ty::Slice(_)) => {
                let Ty::Slice(et) = &**inner else {
                    unreachable!()
                };
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                let et = (**et).clone();
                self.hash_seq(p, n, &et, h);
            }
            Ty::Ref(_, inner) if !self.cg.is_fat(t) => {
                let p = self.load(types::I64, a, 0);
                let inner = (**inner).clone();
                self.hash_into(p, &inner, h);
            }
            Ty::Prim(PrimTy::F32 | PrimTy::F64) => {
                let st = self.cg.scalar(t).unwrap();
                let x = self.load(st, a, 0);
                // +0.0 == -0.0, so they must hash alike.
                let z = if st == types::F64 {
                    self.b.ins().f64const(0.0)
                } else {
                    self.b.ins().f32const(0.0)
                };
                let x = self.b.ins().fadd(x, z);
                let it = if st == types::F64 {
                    types::I64
                } else {
                    types::I32
                };
                let bits = self
                    .b
                    .ins()
                    .bitcast(it, cranelift_codegen::ir::MemFlags::new(), x);
                let bits = if it == types::I64 {
                    bits
                } else {
                    self.b.ins().uextend(types::I64, bits)
                };
                self.call_rt("kn_hash_u64", &[h, bits]);
            }
            _ if self.cg.scalar(t).is_some()
                && !self.cg.is_lang(t, "Box")
                && !matches!(t, Ty::Adt(..)) =>
            {
                let st = self.cg.scalar(t).unwrap();
                let x = self.load(st, a, 0);
                let x = if st == types::I64 {
                    x
                } else if Self::is_signed(t) {
                    self.b.ins().sextend(types::I64, x)
                } else {
                    self.b.ins().uextend(types::I64, x)
                };
                self.call_rt("kn_hash_u64", &[h, x]);
            }
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let x = self.b.ins().iadd_imm(a, off as i64);
                    self.hash_into(x, et, h);
                }
            }
            Ty::Array(et, n) => {
                let n = self.iconst(*n as i64);
                let et = (**et).clone();
                self.hash_seq(a, n, &et, h);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Box") => {
                let p = self.load(types::I64, a, 0);
                let inner = args[0].clone();
                self.hash_into(p, &inner, h);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Vec") => {
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
                let et = args[0].clone();
                self.hash_seq(p, n, &et, h);
            }
            Ty::Adt(..) => {
                if let Some(id) = self.cg.user_impl(t, "Hash", "hash") {
                    let out = self.slot(1, 1);
                    let sa = self.slot(8, 8);
                    self.store(a, sa, 0);
                    let sh = self.slot(8, 8);
                    self.store(h, sh, 0);
                    self.call_fn(id, &[out, sa, sh]);
                    return;
                }
                let id = self.cg.hash_glue_for(t);
                self.call_fn(id, &[h, a]);
            }
            _ => {
                let n = self.cg.typed.fmt_ty(t);
                self.error(format!("cannot hash values of type `{n}`"));
            }
        }
    }

    /// Field-by-field hash of an ADT (the body of its hash glue).
    fn hash_fields(&mut self, a: Value, t: &Ty, h: Value) {
        let variants = self.cg.variants_of(t);
        let is_enum = variants.first().is_some_and(|v| v.is_some());
        if !is_enum {
            for (n, ft) in self.cg.typed.adt_fields(t, None) {
                let (off, _) = self.cg.field(t, &n);
                let x = self.b.ins().iadd_imm(a, off as i64);
                self.hash_into(x, &ft, h);
            }
            return;
        }
        let tag = self.load(types::I32, a, 0);
        let tag64 = self.b.ins().uextend(types::I64, tag);
        self.call_rt("kn_hash_u64", &[h, tag64]);
        let t = t.clone();
        for (vi, v) in variants.into_iter().enumerate() {
            let fields = self.cg.typed.adt_fields(&t, v);
            if fields.is_empty() {
                continue;
            }
            let prefix = v
                .map(|v| format!("{}.", self.cg.r.def(v).name))
                .unwrap_or_default();
            let is = self.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
            self.when(is, |cx| {
                for (n, ft) in &fields {
                    let (off, _) = cx.cg.field(&t, &format!("{prefix}{n}"));
                    let x = cx.b.ins().iadd_imm(a, off as i64);
                    cx.hash_into(x, ft, h);
                }
            });
        }
    }
}

/// Debug glue: the structural `Debug::fmt_debug` behind derived and built-in
/// impls. It writes to a `std::fmt::Formatter`, whose `out: String` is at
/// offset 0 (see `KnFormatter` in the runtime).
impl Codegen<'_, '_> {
    pub(crate) fn debug_glue_for(&mut self, t: &Ty) -> FuncId {
        let key = format!("{t:?}");
        if let Some(id) = self.debug_glue.get(&key) {
            return *id;
        }
        let sig = self.ptr_sig(2);
        self.counter += 1;
        let name = format!("kn_debug{}", self.counter);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare glue");
        self.debug_glue.insert(key, id);
        self.queue.push_back(Work::DebugGlue { id, ty: t.clone() });
        id
    }

    pub(crate) fn define_debug_glue(&mut self, id: FuncId, t: &Ty) {
        let t = t.clone();
        self.with_glue(id, 2, |cx, ps| cx.debug_fields(ps[0], ps[1], &t));
    }
}

impl FnCx<'_, '_, '_, '_> {
    fn fmt_text(&mut self, f: Value, text: &str) {
        let (p, n) = self.str_lit(text);
        self.call_rt("kn_string_push", &[f, p, n]);
    }

    fn fmt_begin(&mut self, f: Value, open: &str) {
        let (p, n) = self.str_lit(open);
        self.call_rt("kn_fmt_begin", &[f, p, n]);
    }

    fn fmt_sep(&mut self, f: Value, first: Value, spaced: bool) {
        let sp = self.b.ins().iconst(types::I32, spaced as i64);
        self.call_rt("kn_fmt_sep", &[f, first, sp]);
    }

    fn fmt_end(&mut self, f: Value, close: &str, empty: Value, spaced: bool) {
        let (p, n) = self.str_lit(close);
        let sp = self.b.ins().iconst(types::I32, spaced as i64);
        self.call_rt("kn_fmt_end", &[f, p, n, empty, sp]);
    }

    /// Writes a fixed list of items, each written by `item(cx, i)`.
    fn debug_group(
        &mut self,
        f: Value,
        open: &str,
        close: &str,
        spaced: bool,
        n: usize,
        item: impl Fn(&mut Self, usize),
    ) {
        self.fmt_begin(f, open);
        for i in 0..n {
            let first = self.b.ins().iconst(types::I32, (i == 0) as i64);
            self.fmt_sep(f, first, spaced);
            item(self, i);
        }
        let empty = self.b.ins().iconst(types::I32, (n == 0) as i64);
        self.fmt_end(f, close, empty, spaced);
    }

    /// Writes `[a, b, ...]` for `len` elements of type `et` at `data`.
    fn debug_seq(&mut self, f: Value, data: Value, len: Value, et: &Ty) {
        self.fmt_begin(f, "[");
        let size = self.cg.layout(et).size;
        let et2 = et.clone();
        self.for_count(len, |cx, i| {
            let first = cx.b.ins().icmp_imm(IntCC::Equal, i, 0);
            let first = cx.b.ins().uextend(types::I32, first);
            cx.fmt_sep(f, first, false);
            let off = cx.b.ins().imul_imm(i, size as i64);
            let x = cx.b.ins().iadd(data, off);
            cx.debug_into(f, x, &et2);
        });
        let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
        let empty = self.b.ins().uextend(types::I32, empty);
        self.fmt_end(f, "]", empty, false);
    }

    /// Structural Debug output of a value ignoring its own `Debug` impl:
    /// the default `Debug::fmt_debug`.
    pub(crate) fn derived_debug(&mut self, f: Value, a: Value, t: &Ty) {
        if matches!(t, Ty::Adt(..))
            && !["Vec", "string", "Box"]
                .iter()
                .any(|n| self.cg.is_lang(t, n))
        {
            let id = self.cg.debug_glue_for(t);
            self.call_fn(id, &[f, a]);
            return;
        }
        self.debug_into(f, a, t);
    }

    /// Writes the Debug form of the value at `a` (type `t`) to formatter `f`.
    pub(crate) fn debug_into(&mut self, f: Value, a: Value, t: &Ty) {
        use kiln_resolve::PrimTy;
        if self.cg.is_lang(t, "string") {
            let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
            self.call_rt("kn_fmt_debug_str", &[f, p, n]);
            return;
        }
        match t {
            Ty::Ref(_, inner) | Ty::Ptr(_, inner) if matches!(**inner, Ty::Prim(PrimTy::Str)) => {
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                self.call_rt("kn_fmt_debug_str", &[f, p, n]);
            }
            Ty::Ref(_, inner) if matches!(**inner, Ty::Slice(_)) => {
                let Ty::Slice(et) = &**inner else {
                    unreachable!()
                };
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 8));
                let et = (**et).clone();
                self.debug_seq(f, p, n, &et);
            }
            Ty::Ref(_, inner) if !self.cg.is_fat(t) => {
                let p = self.load(types::I64, a, 0);
                let inner = (**inner).clone();
                self.debug_into(f, p, &inner);
            }
            Ty::Prim(PrimTy::Char) => {
                let v = self.load(types::I32, a, 0);
                self.call_rt("kn_fmt_debug_char", &[f, v]);
            }
            Ty::Prim(PrimTy::F32 | PrimTy::F64) => {
                let st = self.cg.scalar(t).unwrap();
                let v = self.load(st, a, 0);
                let v = if st == types::F32 {
                    self.b.ins().fpromote(types::F64, v)
                } else {
                    v
                };
                self.call_rt("kn_fmt_debug_f64", &[f, v]);
            }
            Ty::Prim(_) | Ty::Unit | Ty::Never => self.fmt_into(f, a, t),
            Ty::Tuple(ts) => {
                let ts = ts.clone();
                let t2 = t.clone();
                self.debug_group(f, "(", ")", false, ts.len(), |cx, i| {
                    let (off, _) = cx.cg.field(&t2, &i.to_string());
                    let x = cx.b.ins().iadd_imm(a, off as i64);
                    cx.debug_into(f, x, &ts[i]);
                });
            }
            Ty::Array(et, n) => {
                let n = self.iconst(*n as i64);
                let et = (**et).clone();
                self.debug_seq(f, a, n, &et);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Box") => {
                let p = self.load(types::I64, a, 0);
                let inner = args[0].clone();
                self.debug_into(f, p, &inner);
            }
            Ty::Adt(_, args) if self.cg.is_lang(t, "Vec") => {
                let (p, n) = (self.load(types::I64, a, 0), self.load(types::I64, a, 16));
                let et = args[0].clone();
                self.debug_seq(f, p, n, &et);
            }
            Ty::Adt(..) => {
                if let Some(id) = self.cg.user_impl(t, "Debug", "fmt_debug") {
                    let out = self.slot(1, 1);
                    let sa = self.slot(8, 8);
                    self.store(a, sa, 0);
                    let sf = self.slot(8, 8);
                    self.store(f, sf, 0);
                    self.call_fn(id, &[out, sa, sf]);
                    return;
                }
                let id = self.cg.debug_glue_for(t);
                self.call_fn(id, &[f, a]);
            }
            Ty::Fn(..) => self.fmt_text(f, "<fn>"),
            Ty::ChanSend(_) | Ty::ChanRecv(_) => self.fmt_text(f, "<chan>"),
            Ty::Ptr(..) => {
                let v = self.load(types::I64, a, 0);
                let (r, u, alt) = (
                    self.b.ins().iconst(types::I32, 16),
                    self.b.ins().iconst(types::I32, 0),
                    self.b.ins().iconst(types::I32, 1),
                );
                self.call_rt("kn_fmt_radix", &[f, v, r, u, alt]);
            }
            Ty::Dyn(_) => self.fmt_text(f, "<dyn>"),
            _ => {
                let n = self.cg.typed.fmt_ty(t);
                self.error(format!("cannot Debug-format values of type `{n}`"));
            }
        }
    }

    /// The body of an ADT's Debug glue: `Name { field: value, .. }`,
    /// `Name(value, ..)`, or `Name`; a range prints as `start..end`.
    fn debug_fields(&mut self, f: Value, a: Value, t: &Ty) {
        if let Ty::Adt(d, args) = t {
            if self.cg.r.is_core(*d) && self.cg.r.def(*d).name == "Range" {
                let et = args[0].clone();
                let (o0, _) = self.cg.field(t, "start");
                let (o1, _) = self.cg.field(t, "end");
                let x = self.b.ins().iadd_imm(a, o0 as i64);
                self.debug_into(f, x, &et);
                self.fmt_text(f, "..");
                let y = self.b.ins().iadd_imm(a, o1 as i64);
                self.debug_into(f, y, &et);
                return;
            }
        }
        let variants = self.cg.variants_of(t);
        let is_enum = variants.first().is_some_and(|v| v.is_some());
        let named = |cx: &Self, v: Option<DefId>| -> bool {
            let d = match (v, t) {
                (Some(v), _) => v,
                (None, Ty::Adt(d, _)) => *d,
                _ => return false,
            };
            let _ = d;
            let fields = cx.cg.typed.adt_fields(t, v);
            fields
                .first()
                .is_some_and(|(n, _)| n.parse::<usize>().is_err())
        };
        let write_one = |cx: &mut Self, v: Option<DefId>, prefix: &str| {
            let name = match (v, t) {
                (Some(v), _) => cx.cg.r.def(v).name.clone(),
                (None, Ty::Adt(d, _)) => cx.cg.r.def(*d).name.clone(),
                _ => String::new(),
            };
            cx.fmt_text(f, &name);
            let fields = cx.cg.typed.adt_fields(t, v);
            if fields.is_empty() {
                return;
            }
            let is_named = named(cx, v);
            let t2 = t.clone();
            let prefix = prefix.to_string();
            let (open, close) = if is_named { (" {", "}") } else { ("(", ")") };
            cx.debug_group(f, open, close, is_named, fields.len(), |cx, i| {
                let (n, ft) = &fields[i];
                if is_named {
                    cx.fmt_text(f, &format!("{n}: "));
                }
                let (off, _) = cx.cg.field(&t2, &format!("{prefix}{n}"));
                let x = cx.b.ins().iadd_imm(a, off as i64);
                cx.debug_into(f, x, ft);
            });
        };
        if !is_enum {
            write_one(self, None, "");
            return;
        }
        let tag = self.load(types::I32, a, 0);
        for (vi, v) in variants.into_iter().enumerate() {
            let prefix = v
                .map(|v| format!("{}.", self.cg.r.def(v).name))
                .unwrap_or_default();
            let is = self.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
            self.when(is, |cx| write_one(cx, v, &prefix));
        }
    }
}
