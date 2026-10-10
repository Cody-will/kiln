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
                span,
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
            .map(|t| self.typed.trait_methods(t))
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
        if self.cg.is_lang(t, "String") {
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

    /// Writes a clone of the value at `src` (type `t`) to `dst`.
    pub(crate) fn clone_into(&mut self, src: Value, dst: Value, t: &Ty) {
        if !self.cg.needs_drop(t) {
            let l = self.cg.layout(t);
            self.copy(dst, src, l.size);
            return;
        }
        if self.cg.is_lang(t, "String") {
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
            Ty::Adt(..) => {
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
            _ => {
                let l = self.cg.layout(t);
                self.copy(dst, src, l.size);
            }
        }
        let _ = flags();
    }
}
