//! Translation of one monomorphized MIR body to Cranelift IR.

use crate::{Abi, Codegen};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    self, types, InstBuilder, MemFlags, StackSlot, StackSlotData, StackSlotKind, TrapCode, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch};
use cranelift_module::{FuncId, Module};
use kiln_mir::{
    AggKind, Body, BodyKind, Callee, CaptureMode, ConstKind, Constant, InterpSeg, LocalKind, Op,
    Operand, Place, Proj, Rvalue, StmtKind, Terminator,
};
use kiln_parser::ast::BinOp as BinOpKind;
use kiln_resolve::PrimTy;
use kiln_typeck::ty::Ty;
use std::collections::HashMap;

pub(crate) struct LocalInfo {
    pub addr: Value,
    pub ty: Ty,
    /// Whether the local holds a value (absent for types without drop glue).
    pub flag: Option<StackSlot>,
    /// Field paths moved out somewhere in the body, each with a flag saying
    /// whether that part is still present (meaningful while `flag` is set).
    pub paths: Vec<(Vec<String>, StackSlot)>,
}

/// The field path of a place with only field projections (`x.a.b` → `["a", "b"]`).
fn field_path(p: &Place) -> Option<Vec<String>> {
    p.proj
        .iter()
        .map(|x| match x {
            Proj::Field(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// What a call resolves to after monomorphization.
pub(crate) enum Target {
    Direct(FuncId),
    /// Method number `k` of the trait object's vtable.
    Virtual(usize),
    Intrinsic(String),
    Missing(String),
}

pub(crate) struct FnCx<'c, 'r, 'a, 'f> {
    pub cg: &'c mut Codegen<'r, 'a>,
    pub b: FunctionBuilder<'f>,
    pub body: Body,
    pub substs: HashMap<String, Ty>,
    pub fn_idx: usize,
    pub locals: Vec<LocalInfo>,
    pub blocks: Vec<ir::Block>,
    pub defer_stack: Option<StackSlot>,
    /// Handle of the task most recently started by a `go` in this function.
    pub last_task: Option<StackSlot>,
    pub span: kiln_lexer::Span,
}

pub(crate) fn flags() -> MemFlags {
    MemFlags::trusted()
}

impl Codegen<'_, '_> {
    pub(crate) fn define_fn(
        &mut self,
        id: FuncId,
        fn_idx: usize,
        body_i: usize,
        substs: HashMap<String, Ty>,
        abi: Abi,
    ) {
        let Some(body) = self.bodies(fn_idx).get(body_i).cloned() else {
            return;
        };
        self.span = body.span;
        if !body.unsupported.is_empty() {
            for (what, span) in &body.unsupported {
                self.error_at(format!("code generation does not support {what}"), *span);
            }
            return;
        }
        let extra = if abi == Abi::Closure { 2 } else { 1 };
        let mut ctx = self.module.make_context();
        ctx.func.signature = self.ptr_sig(body.arg_count + extra);
        let mut fbc = FunctionBuilderContext::new();
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let span = body.span;
            let mut cx = FnCx {
                cg: self,
                b,
                body,
                substs,
                fn_idx,
                locals: vec![],
                blocks: vec![],
                defer_stack: None,
                last_task: None,
                span,
            };
            cx.translate(abi);
            cx.b.seal_all_blocks();
            cx.b.finalize();
        }
        if let Err(e) = self.module.define_function(id, &mut ctx) {
            self.fatal(format!("internal code generation error: {e:?}"));
        }
    }

    pub(crate) fn define_shim(&mut self, id: FuncId, target: FuncId, nargs: usize) {
        let mut ctx = self.module.make_context();
        ctx.func.signature = self.ptr_sig(nargs + 2);
        let mut fbc = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let e = b.create_block();
            b.append_block_params_for_function_params(e);
            b.switch_to_block(e);
            let ps = b.block_params(e).to_vec();
            let mut args = vec![ps[0]];
            args.extend(&ps[2..]);
            let f = self.module.declare_func_in_func(target, b.func);
            b.ins().call(f, &args);
            b.ins().return_(&[]);
            b.seal_all_blocks();
            b.finalize();
        }
        if let Err(e) = self.module.define_function(id, &mut ctx) {
            self.fatal(format!("internal code generation error: {e:?}"));
        }
    }

    /// Defines the C `main` that calls the user's `main` and turns an `Err` into exit code 1.
    pub(crate) fn define_entry(&mut self, main_idx: usize, user_main: FuncId) {
        let (_, ret) = self.typed.signature(main_idx);
        let mut sig = self.module.make_signature();
        sig.params.push(ir::AbiParam::new(types::I32));
        sig.params.push(ir::AbiParam::new(types::I64));
        sig.returns.push(ir::AbiParam::new(types::I32));
        let id =
            match self
                .module
                .declare_function("kn_main", cranelift_module::Linkage::Export, &sig)
            {
                Ok(id) => id,
                Err(e) => return self.fatal(format!("could not declare main: {e}")),
            };
        let l = self.layout(&ret);
        let is_result = self.is_lang(&ret, "Result");
        let err_info = if is_result {
            let (off, et) = self.field(&ret, "Err.0");
            Some((off, et))
        } else {
            None
        };
        let msg = self.string_data("main returned an error");
        let mut ctx = self.module.make_context();
        ctx.func.signature = sig;
        let mut fbc = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let e = b.create_block();
            b.append_block_params_for_function_params(e);
            b.switch_to_block(e);
            let slot = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                l.size.max(8),
                3,
            ));
            let addr = b.ins().stack_addr(types::I64, slot, 0);
            let f = self.module.declare_func_in_func(user_main, b.func);
            b.ins().call(f, &[addr]);
            match err_info {
                Some((off, et)) => {
                    let tag = b.ins().load(types::I32, flags(), addr, 0);
                    let (okb, errb) = (b.create_block(), b.create_block());
                    b.ins().brif(tag, errb, &[], okb, &[]);
                    b.switch_to_block(okb);
                    let z = b.ins().iconst(types::I32, 0);
                    b.ins().return_(&[z]);
                    b.switch_to_block(errb);
                    let pe = self
                        .module
                        .declare_func_in_func(self.rt["kn_main_error"], b.func);
                    let p = b.ins().iadd_imm(addr, off as i64);
                    let (data, len) = if self.is_lang(&et, "String") {
                        (
                            b.ins().load(types::I64, flags(), p, 0),
                            b.ins().load(types::I64, flags(), p, 16),
                        )
                    } else if matches!(&et, Ty::Ref(_, x) if **x == Ty::Prim(PrimTy::Str)) {
                        (
                            b.ins().load(types::I64, flags(), p, 0),
                            b.ins().load(types::I64, flags(), p, 8),
                        )
                    } else {
                        let gv = self.module.declare_data_in_func(msg, b.func);
                        let d = b.ins().global_value(types::I64, gv);
                        (
                            d,
                            b.ins()
                                .iconst(types::I64, "main returned an error".len() as i64),
                        )
                    };
                    b.ins().call(pe, &[data, len]);
                    let one = b.ins().iconst(types::I32, 1);
                    b.ins().return_(&[one]);
                }
                None => {
                    let z = b.ins().iconst(types::I32, 0);
                    b.ins().return_(&[z]);
                }
            }
            b.seal_all_blocks();
            b.finalize();
        }
        if let Err(e) = self.module.define_function(id, &mut ctx) {
            self.fatal(format!("internal code generation error: {e:?}"));
        }
    }
}

impl FnCx<'_, '_, '_, '_> {
    // ----- helpers --------------------------------------------------------------------

    pub(crate) fn subst(&self, t: &Ty) -> Ty {
        t.subst(&self.substs)
    }

    pub(crate) fn error(&mut self, msg: impl Into<String>) {
        let span = self.span;
        self.cg.error_at(msg, span);
    }

    pub(crate) fn slot(&mut self, size: u32, align: u32) -> Value {
        let shift = align.max(1).trailing_zeros() as u8;
        let s = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size.max(1),
            shift,
        ));
        self.b.ins().stack_addr(types::I64, s, 0)
    }

    pub(crate) fn slot_for(&mut self, t: &Ty) -> Value {
        let l = self.cg.layout(t);
        self.slot(l.size, l.align)
    }

    pub(crate) fn iconst(&mut self, v: i64) -> Value {
        self.b.ins().iconst(types::I64, v)
    }

    pub(crate) fn load(&mut self, t: ir::Type, addr: Value, off: i32) -> Value {
        self.b.ins().load(t, flags(), addr, off)
    }

    pub(crate) fn store(&mut self, v: Value, addr: Value, off: i32) {
        self.b.ins().store(flags(), v, addr, off);
    }

    pub(crate) fn copy(&mut self, dst: Value, src: Value, size: u32) {
        if size == 0 {
            return;
        }
        let cfg = self.cg.module.target_config();
        self.b
            .emit_small_memory_copy(cfg, dst, src, size as u64, 1, 1, true, flags());
    }

    pub(crate) fn call_rt(&mut self, name: &str, args: &[Value]) -> Option<Value> {
        let id = self.cg.rt[name];
        let f = self.cg.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call).first().copied()
    }

    pub(crate) fn call_fn(&mut self, id: FuncId, args: &[Value]) {
        let f = self.cg.module.declare_func_in_func(id, self.b.func);
        self.b.ins().call(f, args);
    }

    /// A pointer to a string literal's bytes and its length.
    pub(crate) fn str_lit(&mut self, s: &str) -> (Value, Value) {
        let id = self.cg.string_data(s);
        let gv = self.cg.module.declare_data_in_func(id, self.b.func);
        let p = self.b.ins().global_value(types::I64, gv);
        let n = self.iconst(s.len() as i64);
        (p, n)
    }

    pub(crate) fn panic_msg(&mut self, msg: &str) {
        let (p, n) = self.str_lit(msg);
        self.call_rt("kn_panic", &[p, n]);
    }

    /// Runs `f` only when `cond` is non-zero.
    pub(crate) fn when(&mut self, cond: Value, f: impl FnOnce(&mut Self)) {
        let (yes, done) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(cond, yes, &[], done, &[]);
        self.b.switch_to_block(yes);
        f(self);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    pub(crate) fn drop_value(&mut self, addr: Value, t: &Ty) {
        if self.cg.needs_drop(t) {
            let g = self.cg.drop_glue_for(t);
            self.call_fn(g, &[addr]);
        }
    }

    fn flag_of(&self, p: &Place) -> Option<StackSlot> {
        if p.through_ref() {
            return None;
        }
        self.locals[p.local.0 as usize].flag
    }

    fn set_flag(&mut self, s: StackSlot, v: i64) {
        let c = self.b.ins().iconst(types::I8, v);
        self.b.ins().stack_store(c, s, 0);
    }

    // ----- body ---------------------------------------------------------------------

    fn translate(&mut self, abi: Abi) {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        let params = self.b.block_params(entry).to_vec();
        let extra = if abi == Abi::Closure { 2 } else { 1 };
        // Closure environments: captures laid out in order.
        let mut env_offsets = HashMap::new();
        if abi == Abi::Closure {
            let caps = self.body.captures.clone();
            let ls: Vec<crate::Layout> = caps
                .iter()
                .map(|(l, m)| {
                    let t = self.subst(&self.body.local(*l).ty);
                    if *m == CaptureMode::Move {
                        self.cg.layout(&t)
                    } else {
                        crate::Layout { size: 8, align: 8 }
                    }
                })
                .collect();
            let (offs, _) = crate::layout::sequential(&ls);
            for ((l, m), off) in caps.iter().zip(offs) {
                env_offsets.insert(*l, (off, *m));
            }
        }
        // Field paths moved out of each local (partial moves).
        let mut moved_paths: HashMap<u32, Vec<Vec<String>>> = HashMap::new();
        for blk in &self.body.blocks {
            for st in &blk.stmts {
                let ops: Vec<&Operand> = match &st.kind {
                    StmtKind::Assign(_, rv) | StmtKind::Eval(rv) => rv.operands(),
                    StmtKind::Spawn { operands, .. } => operands.iter().collect(),
                    _ => vec![],
                };
                for o in ops {
                    if let Operand::Move(p) = o {
                        if let Some(path) = field_path(p).filter(|x| !x.is_empty()) {
                            let e = moved_paths.entry(p.local.0).or_default();
                            if !e.contains(&path) {
                                e.push(path);
                            }
                        }
                    }
                }
            }
        }
        for i in 0..self.body.locals.len() {
            let decl = self.body.locals[i].clone();
            let ty = self.subst(&decl.ty);
            let addr = match decl.kind {
                LocalKind::Return => params[0],
                LocalKind::Arg => params[extra + i - 1],
                LocalKind::Upvar => match env_offsets.get(&kiln_mir::Local(i as u32)) {
                    Some((off, CaptureMode::Move)) => self.b.ins().iadd_imm(params[1], *off as i64),
                    Some((off, _)) => self.load(types::I64, params[1], *off as i32),
                    None => self.slot_for(&ty),
                },
                _ => self.slot_for(&ty),
            };
            // A task owns its moved captures, so they get drop flags too.
            let owned_upvar = decl.kind == LocalKind::Upvar
                && self.body.kind == BodyKind::GoBlock
                && matches!(
                    env_offsets.get(&kiln_mir::Local(i as u32)),
                    Some((_, CaptureMode::Move))
                );
            let flag = if (matches!(decl.kind, LocalKind::Return | LocalKind::Upvar)
                && !owned_upvar)
                || !self.cg.needs_drop(&ty)
            {
                None
            } else {
                let s = self.b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    1,
                    0,
                ));
                let v = self.b.ins().iconst(
                    types::I8,
                    (decl.kind == LocalKind::Arg || owned_upvar) as i64,
                );
                self.b.ins().stack_store(v, s, 0);
                Some(s)
            };
            let mut paths = Vec::new();
            if flag.is_some() {
                for path in moved_paths.get(&(i as u32)).into_iter().flatten() {
                    let s = self.b.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        1,
                        0,
                    ));
                    let v = self.b.ins().iconst(types::I8, 1);
                    self.b.ins().stack_store(v, s, 0);
                    paths.push((path.clone(), s));
                }
            }
            self.locals.push(LocalInfo {
                addr,
                ty,
                flag,
                paths,
            });
        }
        let uses_defer = self.body.blocks.iter().any(|b| matches!(b.term, Terminator::DeferDispatch { ref targets, .. } if !targets.is_empty()));
        if uses_defer {
            let s = self.b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                32,
                3,
            ));
            let z = self.iconst(0);
            for off in [0, 8, 16, 24] {
                self.b.ins().stack_store(z, s, off);
            }
            self.defer_stack = Some(s);
        }
        self.blocks = (0..self.body.blocks.len())
            .map(|_| self.b.create_block())
            .collect();
        let first = self.blocks[0];
        self.b.ins().jump(first, &[]);
        for bi in 0..self.body.blocks.len() {
            let blk = self.blocks[bi];
            self.b.switch_to_block(blk);
            let stmts = self.body.blocks[bi].stmts.clone();
            for s in &stmts {
                self.span = s.span;
                self.stmt(&s.kind);
            }
            let term = self.body.blocks[bi].term.clone();
            self.span = self.body.blocks[bi].term_span;
            self.terminator(&term);
        }
    }

    fn stmt(&mut self, s: &StmtKind) {
        match s {
            StmtKind::Assign(p, rv) => self.assign(p, rv),
            StmtKind::Eval(rv) => {
                let t = self.rvalue_ty(rv);
                let scratch = self.slot_for(&t);
                self.rvalue(rv, scratch, &t);
                if !matches!(rv, Rvalue::Use(Operand::Copy(_))) {
                    self.drop_value(scratch, &t);
                }
            }
            StmtKind::Read(_) => {}
            StmtKind::StorageDead(l) => {
                let info = &self.locals[l.0 as usize];
                if let Some(f) = info.flag {
                    let (addr, ty) = (info.addr, info.ty.clone());
                    let li = l.0 as usize;
                    let v = self.b.ins().stack_load(types::I8, f, 0);
                    self.when(v, |cx| cx.drop_elab(addr, &ty, li, &[]));
                    self.set_flag(f, 0);
                }
            }
            StmtKind::DeferPush(n, rec) => {
                let s = self.defer_stack.expect("defer stack");
                let args = match rec {
                    Some(r) => {
                        // Move the saved arguments to the heap.
                        let (addr, ty) = {
                            let i = &self.locals[r.0 as usize];
                            (i.addr, i.ty.clone())
                        };
                        let l = self.cg.layout(&ty);
                        let (sz, al) = (
                            self.iconst(l.size.max(1) as i64),
                            self.iconst(l.align as i64),
                        );
                        let p = self.call_rt("kn_alloc", &[sz, al]).unwrap();
                        self.copy(p, addr, l.size);
                        if let Some(f) = self.locals[r.0 as usize].flag {
                            self.set_flag(f, 0);
                        }
                        p
                    }
                    None => self.iconst(0),
                };
                let a = self.b.ins().stack_addr(types::I64, s, 0);
                let id = self.b.ins().iconst(types::I32, *n as i64);
                self.call_rt("kn_defer_push", &[a, id, args]);
            }
            StmtKind::DeferLoad(r) => {
                let s = self.defer_stack.expect("defer stack");
                let p = self.b.ins().stack_load(types::I64, s, 24);
                let li = r.0 as usize;
                let (addr, ty, flag, paths) = {
                    let i = &self.locals[li];
                    (
                        i.addr,
                        i.ty.clone(),
                        i.flag,
                        i.paths.iter().map(|x| x.1).collect::<Vec<_>>(),
                    )
                };
                let l = self.cg.layout(&ty);
                if let Some(f) = flag {
                    // Arguments kept from an earlier run of the same `defer` (printing borrows them).
                    let v = self.b.ins().stack_load(types::I8, f, 0);
                    self.when(v, |cx| cx.drop_elab(addr, &ty, li, &[]));
                    self.set_flag(f, 1);
                }
                for ps in paths {
                    self.set_flag(ps, 1);
                }
                self.copy(addr, p, l.size);
                let (sz, al) = (
                    self.iconst(l.size.max(1) as i64),
                    self.iconst(l.align as i64),
                );
                self.call_rt("kn_free", &[p, sz, al]);
            }
            StmtKind::Spawn {
                kind,
                operands,
                body,
                ..
            } => {
                if *kind != kiln_mir::SpawnKind::Scoped {
                    self.spawn(operands, *body);
                }
            }
        }
    }

    /// `go`: moves the captures into a heap environment and starts the task
    /// body (a closure-ABI instance) on the scheduler. The handle is read by
    /// the following `go_handle` intrinsic.
    fn spawn(&mut self, ops: &[Operand], body: kiln_lexer::Span) {
        let bi = self
            .cg
            .bodies(self.fn_idx)
            .iter()
            .position(|b| b.kind == BodyKind::GoBlock && b.span == body);
        let Some(bi) = bi else {
            return self.error("internal error: task body not found");
        };
        let ret_ty = self.cg.bodies(self.fn_idx)[bi].locals[0].ty.clone();
        let ret_ty = self.subst(&ret_ty);
        let id = self
            .cg
            .instance(self.fn_idx, bi, self.substs.clone(), Abi::Closure);
        let tys: Vec<Ty> = ops.iter().map(|o| self.op_ty(o)).collect();
        let ls: Vec<crate::Layout> = tys.iter().map(|t| self.cg.layout(t)).collect();
        let (offs, l) = crate::layout::sequential(&ls);
        let size = self.iconst(l.size as i64);
        let align = self.iconst(l.align as i64);
        let env = self.call_rt("kn_alloc", &[size, align]).unwrap();
        for (o, off) in ops.iter().zip(offs) {
            let a = self.b.ins().iadd_imm(env, off as i64);
            self.store_op(o, a);
        }
        let f = self.cg.module.declare_func_in_func(id, self.b.func);
        let code = self.b.ins().func_addr(types::I64, f);
        let ret_size = self.cg.layout(&ret_ty).size as i64;
        let ret_size = self.iconst(ret_size);
        let drop = self.drop_fn_ptr(&ret_ty);
        let h = self
            .call_rt("kn_go", &[code, env, size, ret_size, drop])
            .unwrap();
        let s = *self.last_task.get_or_insert_with(|| {
            self.b
                .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3))
        });
        self.b.ins().stack_store(h, s, 0);
    }

    /// The drop glue of `t` as a function pointer (0 if `t` needs no drop).
    pub(crate) fn drop_fn_ptr(&mut self, t: &Ty) -> Value {
        if !self.cg.needs_drop(t) {
            return self.iconst(0);
        }
        let g = self.cg.drop_glue_for(t);
        let f = self.cg.module.declare_func_in_func(g, self.b.func);
        self.b.ins().func_addr(types::I64, f)
    }

    fn assign(&mut self, p: &Place, rv: &Rvalue) {
        let pty = self.place_ty(p);
        let l = self.cg.layout(&pty);
        let scratch = self.slot(l.size, l.align);
        self.rvalue(rv, scratch, &pty);
        let (dest, _) = self.place_addr(p);
        if self.cg.needs_drop(&pty) {
            let has_deref = p
                .proj
                .iter()
                .any(|x| matches!(x, Proj::Deref(_) | Proj::Index(_)));
            match self.flag_of(p) {
                Some(f) if !has_deref => {
                    let v = self.b.ins().stack_load(types::I8, f, 0);
                    let li = p.local.0 as usize;
                    let path = field_path(p).unwrap_or_default();
                    self.when(v, |cx| cx.drop_elab(dest, &pty, li, &path));
                }
                _ if has_deref => self.drop_value(dest, &pty),
                _ => {}
            }
        }
        self.copy(dest, scratch, l.size);
        if p.proj.is_empty() {
            if let Some(f) = self.locals[p.local.0 as usize].flag {
                self.set_flag(f, 1);
            }
        }
        // The assigned part (and everything inside it) is present again.
        if let Some(path) = field_path(p) {
            let restored: Vec<StackSlot> = self.locals[p.local.0 as usize]
                .paths
                .iter()
                .filter(|(q, _)| q.starts_with(&path))
                .map(|(_, s)| *s)
                .collect();
            for s in restored {
                self.set_flag(s, 1);
            }
        }
    }

    fn terminator(&mut self, t: &Terminator) {
        match t {
            Terminator::Goto(b) => {
                let blk = self.blocks[b.0 as usize];
                self.b.ins().jump(blk, &[]);
            }
            Terminator::SwitchBool { cond, t, f } => {
                let c = self.op_scalar(cond);
                let (tb, fb) = (self.blocks[t.0 as usize], self.blocks[f.0 as usize]);
                self.b.ins().brif(c, tb, &[], fb, &[]);
            }
            Terminator::Return => {
                self.b.ins().return_(&[]);
            }
            Terminator::Unreachable => {
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
            Terminator::Branch(_) => {
                self.error("code generation does not support `select` yet");
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
            Terminator::DeferDispatch { targets, done } => {
                let done = self.blocks[done.0 as usize];
                let Some(s) = self.defer_stack else {
                    self.b.ins().jump(done, &[]);
                    return;
                };
                let a = self.b.ins().stack_addr(types::I64, s, 0);
                let id = self.call_rt("kn_defer_pop", &[a]).unwrap();
                let bad = self.b.create_block();
                let mut sw = Switch::new();
                for (i, t) in targets.iter().enumerate() {
                    if let Some(t) = t {
                        sw.set_entry(i as u128, self.blocks[t.0 as usize]);
                    }
                }
                let neg = self.b.ins().icmp_imm(IntCC::SignedLessThan, id, 0);
                let dispatch = self.b.create_block();
                self.b.ins().brif(neg, done, &[], dispatch, &[]);
                self.b.switch_to_block(dispatch);
                sw.emit(&mut self.b, id, bad);
                self.b.switch_to_block(bad);
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
        }
    }

    // ----- places ------------------------------------------------------------------

    pub(crate) fn place_ty(&mut self, p: &Place) -> Ty {
        let mut t = self.locals[p.local.0 as usize].ty.clone();
        for pr in &p.proj {
            t = match pr {
                Proj::Deref(_) => self.cg.pointee(&t).unwrap_or(Ty::Unit),
                Proj::Field(n) => self.cg.field(&t, n).1,
                Proj::Index(_) => self.elem_of(&t),
            };
        }
        t
    }

    fn elem_of(&self, t: &Ty) -> Ty {
        match t {
            Ty::Adt(_, a) if self.cg.is_lang(t, "Vec") => a[0].clone(),
            Ty::Slice(x) | Ty::Array(x, _) => (**x).clone(),
            _ => Ty::Unit,
        }
    }

    /// Address (and fat-pointer metadata for unsized places) of a place.
    pub(crate) fn place_addr(&mut self, p: &Place) -> (Value, Option<Value>) {
        let info = &self.locals[p.local.0 as usize];
        let (mut addr, mut t) = (info.addr, info.ty.clone());
        let mut meta = None;
        for pr in &p.proj {
            match pr {
                Proj::Deref(_) => {
                    let inner = self.cg.pointee(&t).unwrap_or(Ty::Unit);
                    let fat = self.cg.is_unsized(&inner);
                    let data = self.load(types::I64, addr, 0);
                    meta = if fat {
                        Some(self.load(types::I64, addr, 8))
                    } else {
                        None
                    };
                    addr = data;
                    t = inner;
                }
                Proj::Field(n) => {
                    let (off, ft) = self.cg.field(&t, n);
                    addr = self.b.ins().iadd_imm(addr, off as i64);
                    t = ft;
                    meta = None;
                }
                Proj::Index(l) => {
                    let li = &self.locals[l.0 as usize];
                    let (ia, it) = (li.addr, li.ty.clone());
                    let st = self.cg.scalar(&it).unwrap_or(types::I64);
                    let mut i = self.load(st, ia, 0);
                    if st.bits() < 64 {
                        i = self.b.ins().uextend(types::I64, i);
                    }
                    let elem = self.elem_of(&t);
                    let (data, len) = if self.cg.is_lang(&t, "Vec") {
                        (
                            self.load(types::I64, addr, 0),
                            self.load(types::I64, addr, 16),
                        )
                    } else if let (Ty::Slice(_), Some(m)) = (&t, meta) {
                        (addr, m)
                    } else if let Ty::Array(_, n) = &t {
                        (addr, self.iconst(*n as i64))
                    } else {
                        self.error("code generation does not support indexing this type yet");
                        (addr, self.iconst(0))
                    };
                    let oob = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
                    self.when(oob, |cx| {
                        cx.call_rt("kn_panic_bounds", &[i, len]);
                    });
                    let size = self.cg.layout(&elem).size as i64;
                    let off = self.b.ins().imul_imm(i, size);
                    addr = self.b.ins().iadd(data, off);
                    t = elem;
                    meta = None;
                }
            }
        }
        (addr, meta)
    }

    // ----- operands ------------------------------------------------------------------

    pub(crate) fn op_ty(&mut self, o: &Operand) -> Ty {
        match o {
            Operand::Copy(p) | Operand::Move(p) => self.place_ty(p),
            Operand::Const(c) => self.subst(&c.ty),
        }
    }

    pub(crate) fn moved(&mut self, p: &Place) {
        if p.through_ref() {
            return;
        }
        match field_path(p) {
            Some(path) if !path.is_empty() => {
                let s = self.locals[p.local.0 as usize]
                    .paths
                    .iter()
                    .find(|(q, _)| *q == path)
                    .map(|(_, s)| *s);
                if let Some(s) = s {
                    self.set_flag(s, 0);
                }
            }
            _ => {
                if let Some(f) = self.flag_of(p) {
                    self.set_flag(f, 0);
                }
            }
        }
    }

    /// Drops the part of local `l` at field path `prefix` (stored at `addr`),
    /// skipping parts that were moved out (drop elaboration for partial moves).
    pub(crate) fn drop_elab(&mut self, addr: Value, t: &Ty, l: usize, prefix: &[String]) {
        let own = self.locals[l]
            .paths
            .iter()
            .find(|(q, _)| q.as_slice() == prefix)
            .map(|(_, s)| *s);
        match own {
            Some(f) => {
                let v = self.b.ins().stack_load(types::I8, f, 0);
                self.when(v, |cx| cx.drop_elab_children(addr, t, l, prefix));
            }
            None => self.drop_elab_children(addr, t, l, prefix),
        }
    }

    fn drop_elab_children(&mut self, addr: Value, t: &Ty, l: usize, prefix: &[String]) {
        let deeper = self.locals[l]
            .paths
            .iter()
            .any(|(q, _)| q.len() > prefix.len() && q.starts_with(prefix));
        if !deeper {
            self.drop_value(addr, t);
            return;
        }
        let sub = |n: String| {
            let mut v = prefix.to_vec();
            v.push(n);
            v
        };
        match t {
            Ty::Tuple(ts) => {
                for (i, et) in ts.clone().iter().enumerate() {
                    let (off, _) = self.cg.field(t, &i.to_string());
                    let a = self.b.ins().iadd_imm(addr, off as i64);
                    self.drop_elab(a, et, l, &sub(i.to_string()));
                }
            }
            Ty::Adt(..) => {
                let variants = self.cg.variants_of(t);
                let is_enum = variants.first().is_some_and(|v| v.is_some());
                for (vi, v) in variants.into_iter().enumerate() {
                    let fields = self.cg.typed.adt_fields(t, v);
                    let pfx = v
                        .map(|v| format!("{}.", self.cg.r.def(v).name))
                        .unwrap_or_default();
                    let body = |cx: &mut Self| {
                        for (n, ft) in &fields {
                            let name = format!("{pfx}{n}");
                            let (off, _) = cx.cg.field(t, &name);
                            let a = cx.b.ins().iadd_imm(addr, off as i64);
                            cx.drop_elab(a, ft, l, &sub(name));
                        }
                    };
                    if is_enum {
                        let tag = self.load(types::I32, addr, 0);
                        let is = self.b.ins().icmp_imm(IntCC::Equal, tag, vi as i64);
                        self.when(is, body);
                    } else {
                        body(self);
                    }
                }
            }
            _ => self.drop_value(addr, t),
        }
    }

    /// Writes the operand's value to `dest`.
    pub(crate) fn store_op(&mut self, o: &Operand, dest: Value) {
        match o {
            Operand::Copy(p) | Operand::Move(p) => {
                let t = self.place_ty(p);
                let (src, _) = self.place_addr(p);
                let size = self.cg.layout(&t).size;
                self.copy(dest, src, size);
                if matches!(o, Operand::Move(_)) {
                    self.moved(p);
                }
            }
            Operand::Const(c) => self.store_const(c, dest),
        }
    }

    /// Address of the operand's value (constants are spilled to a slot).
    pub(crate) fn op_addr(&mut self, o: &Operand) -> Value {
        match o {
            Operand::Copy(p) | Operand::Move(p) => {
                let (a, _) = self.place_addr(p);
                if matches!(o, Operand::Move(_)) {
                    self.moved(p);
                }
                a
            }
            Operand::Const(c) => {
                let t = self.subst(&c.ty);
                let s = self.slot_for(&t);
                self.store_const(c, s);
                s
            }
        }
    }

    /// The operand's value as a scalar.
    pub(crate) fn op_scalar(&mut self, o: &Operand) -> Value {
        let t = self.op_ty(o);
        if let (Operand::Const(c), Some(st)) = (o, self.cg.scalar(&t)) {
            if let Some(v) = self.const_scalar(c, st) {
                return v;
            }
        }
        let a = self.op_addr(o);
        let st = self.cg.scalar(&t).unwrap_or(types::I64);
        self.load(st, a, 0)
    }

    fn const_scalar(&mut self, c: &Constant, st: ir::Type) -> Option<Value> {
        Some(match &c.kind {
            ConstKind::Int(v) if st.is_int() && st.bits() <= 64 => {
                self.b.ins().iconst(st, *v as i64)
            }
            ConstKind::Int(v) if st == types::F64 => self.b.ins().f64const(*v as f64),
            ConstKind::Int(v) if st == types::F32 => self.b.ins().f32const(*v as f32),
            ConstKind::Float(v) if st == types::F64 => self.b.ins().f64const(*v),
            ConstKind::Float(v) if st == types::F32 => self.b.ins().f32const(*v as f32),
            ConstKind::Bool(v) => self.b.ins().iconst(types::I8, *v as i64),
            ConstKind::Char(v) => self.b.ins().iconst(types::I32, *v as i64),
            _ => return None,
        })
    }

    fn store_const(&mut self, c: &Constant, dest: Value) {
        let t = self.subst(&c.ty);
        match &c.kind {
            ConstKind::Unit => {}
            ConstKind::Str(s) => {
                let (p, n) = self.str_lit(s);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            ConstKind::Fn(i, substs) => {
                let substs: HashMap<String, Ty> = substs
                    .iter()
                    .map(|(k, v)| (k.clone(), self.subst(v)))
                    .collect();
                match self.resolve(*i, substs) {
                    Target::Direct(id) => {
                        let nargs = match &t {
                            Ty::Fn(ps, _) => ps.len(),
                            _ => 0,
                        };
                        let shim = self.cg.shim(id, nargs);
                        let f = self.cg.module.declare_func_in_func(shim, self.b.func);
                        let code = self.b.ins().func_addr(types::I64, f);
                        let z = self.iconst(0);
                        self.store(code, dest, 0);
                        self.store(z, dest, 8);
                    }
                    _ => self.error(
                        "code generation does not support using this function as a value yet",
                    ),
                }
            }
            _ => match self.cg.scalar(&t) {
                Some(st) => match self.const_scalar(c, st) {
                    Some(v) => self.store(v, dest, 0),
                    None => self.error("code generation does not support this constant yet"),
                },
                None => self.error("code generation does not support this constant yet"),
            },
        }
    }

    // ----- rvalues -----------------------------------------------------------------

    fn rvalue_ty(&mut self, rv: &Rvalue) -> Ty {
        match rv {
            Rvalue::Use(o) => self.op_ty(o),
            Rvalue::Coerce(_, t) => self.subst(t),
            Rvalue::Call { callee, args, .. } => self.call_ret_ty(callee, args),
            Rvalue::Pure(..) | Rvalue::Ref { .. } | Rvalue::Aggregate(..) => Ty::Unit,
        }
    }

    /// Evaluates `rv` into `dest` (of type `dt`).
    fn rvalue(&mut self, rv: &Rvalue, dest: Value, dt: &Ty) {
        match rv {
            Rvalue::Use(o) => self.store_op(o, dest),
            Rvalue::Ref { place, .. } => {
                let (a, meta) = self.place_addr(place);
                self.store(a, dest, 0);
                if let Some(m) = meta {
                    self.store(m, dest, 8);
                }
            }
            Rvalue::Aggregate(kind, ops) => self.aggregate(kind, ops, dest, dt),
            Rvalue::Call { callee, args, .. } => self.call(callee, args, dest, dt),
            Rvalue::Pure(op, ops) => self.pure(op, ops, dest, dt),
            Rvalue::Coerce(o, target) => {
                let target = self.subst(target);
                self.coerce(o, &target, dest);
            }
        }
    }

    fn aggregate(&mut self, kind: &AggKind, ops: &[Operand], dest: Value, dt: &Ty) {
        match kind {
            AggKind::Tuple => {
                for (i, o) in ops.iter().enumerate() {
                    let (off, _) = self.cg.field(dt, &i.to_string());
                    let a = self.b.ins().iadd_imm(dest, off as i64);
                    self.store_op(o, a);
                }
            }
            AggKind::Struct(_, names) => {
                for (n, o) in names.iter().zip(ops) {
                    let (off, _) = self.cg.field(dt, n);
                    let a = self.b.ins().iadd_imm(dest, off as i64);
                    self.store_op(o, a);
                }
            }
            AggKind::Variant { def, index, fields } => {
                let tag = self.b.ins().iconst(types::I32, *index as i64);
                self.store(tag, dest, 0);
                let vname = self.cg.r.def(self.cg.r.variants(*def)[*index]).name.clone();
                for (n, o) in fields.iter().zip(ops) {
                    let (off, _) = self.cg.field(dt, &format!("{vname}.{n}"));
                    let a = self.b.ins().iadd_imm(dest, off as i64);
                    self.store_op(o, a);
                }
            }
            AggKind::Range {
                has_start: true,
                has_end: true,
                ..
            } => {
                for (n, o) in ["start", "end"].iter().zip(ops) {
                    let (off, _) = self.cg.field(dt, n);
                    let a = self.b.ins().iadd_imm(dest, off as i64);
                    self.store_op(o, a);
                }
            }
            AggKind::Range { .. } => {
                self.error("code generation supports only `a..b` and `a..=b` ranges")
            }
            AggKind::Array => {
                let size = self.cg.layout(&self.elem_of(dt)).size;
                for (i, o) in ops.iter().enumerate() {
                    let a = self.b.ins().iadd_imm(dest, (i as u32 * size) as i64);
                    self.store_op(o, a);
                }
            }
            AggKind::ArrayRepeat(n) => {
                // The element is `Copy` (checked by typeck) unless n <= 1.
                let et = self.elem_of(dt);
                let size = self.cg.layout(&et).size;
                let tmp = self.slot_for(&et);
                self.store_op(&ops[0], tmp);
                self.for_each_index(dest, *n, size, |cx, e| cx.copy(e, tmp, size));
                if *n == 0 {
                    self.drop_value(tmp, &et);
                }
            }
            AggKind::Closure(span) => {
                let bi = self
                    .cg
                    .bodies(self.fn_idx)
                    .iter()
                    .position(|b| b.kind == BodyKind::Closure && b.span == *span);
                let Some(bi) = bi else {
                    return self.error("internal error: closure body not found");
                };
                let id = self
                    .cg
                    .instance(self.fn_idx, bi, self.substs.clone(), Abi::Closure);
                // Environment: captured values (by value or pointer) in order.
                let tys: Vec<Ty> = ops.iter().map(|o| self.op_ty(o)).collect();
                let ls: Vec<crate::Layout> = tys.iter().map(|t| self.cg.layout(t)).collect();
                let (offs, l) = crate::layout::sequential(&ls);
                let size = self.iconst(l.size as i64);
                let align = self.iconst(l.align as i64);
                let env = self.call_rt("kn_alloc", &[size, align]).unwrap();
                for (o, off) in ops.iter().zip(offs) {
                    let a = self.b.ins().iadd_imm(env, off as i64);
                    self.store_op(o, a);
                }
                let f = self.cg.module.declare_func_in_func(id, self.b.func);
                let code = self.b.ins().func_addr(types::I64, f);
                self.store(code, dest, 0);
                self.store(env, dest, 8);
            }
        }
    }

    fn coerce(&mut self, o: &Operand, target: &Ty, dest: Value) {
        let src_ty = self.op_ty(o);
        let from = self.cg.pointee(&src_ty);
        let to = self.cg.pointee(target);
        let (Some(from), Some(to)) = (from, to) else {
            return self.store_op(o, dest);
        };
        let p = self.op_scalar_or_first_word(o, &src_ty);
        match (&from, &to) {
            (_, Ty::Dyn(traits)) if !matches!(from, Ty::Dyn(_)) => {
                let vt = self.cg.vtable(&from, traits.first().copied());
                let gv = self.cg.module.declare_data_in_func(vt, self.b.func);
                let v = self.b.ins().global_value(types::I64, gv);
                self.store(p, dest, 0);
                self.store(v, dest, 8);
            }
            (f, Ty::Prim(PrimTy::Str)) if self.cg.is_lang(f, "String") => {
                let d = self.load(types::I64, p, 0);
                let n = self.load(types::I64, p, 16);
                self.store(d, dest, 0);
                self.store(n, dest, 8);
            }
            (Ty::Array(_, n), Ty::Slice(_)) => {
                let n = self.iconst(*n as i64);
                self.store(p, dest, 0);
                self.store(n, dest, 8);
            }
            (f, Ty::Slice(_)) if self.cg.is_lang(f, "Vec") => {
                let d = self.load(types::I64, p, 0);
                let n = self.load(types::I64, p, 16);
                self.store(d, dest, 0);
                self.store(n, dest, 8);
            }
            (f, _) if self.cg.is_lang(f, "Box") && !self.cg.is_lang(&to, "Box") => {
                // `&Box<T>` → `&T`
                let inner = self.load(types::I64, p, 0);
                self.store(inner, dest, 0);
                if self.cg.is_fat(f) {
                    let m = self.load(types::I64, p, 8);
                    self.store(m, dest, 8);
                }
            }
            _ => self.store_op(o, dest),
        }
    }

    /// The data pointer of a pointer-typed operand.
    fn op_scalar_or_first_word(&mut self, o: &Operand, t: &Ty) -> Value {
        if self.cg.scalar(t).is_some() {
            self.op_scalar(o)
        } else {
            let a = self.op_addr(o);
            self.load(types::I64, a, 0)
        }
    }

    // ----- calls -------------------------------------------------------------------

    /// Resolves a (substituted) callee to a concrete function.
    pub(crate) fn resolve(&mut self, idx: usize, substs: HashMap<String, Ty>) -> Target {
        let r = self.cg.r;
        let f = &r.fns[idx];
        if let (Some(trait_def), None) = (f.trait_def, f.impl_idx) {
            let self_ty = substs.get("Self").cloned().unwrap_or(Ty::Unit);
            if let Ty::Dyn(_) = self_ty {
                let k = self
                    .cg
                    .typed
                    .trait_methods(trait_def)
                    .iter()
                    .position(|m| *m == idx)
                    .unwrap_or(0);
                return Target::Virtual(k);
            }
            return match self.cg.typed.resolve_trait_method(idx, &substs) {
                Some((i2, s2)) if i2 != idx || r.fns[i2].decl.body.is_some() => {
                    if i2 == idx {
                        Target::Direct(self.cg.instance(i2, 0, s2, Abi::Plain))
                    } else {
                        let s2 = s2.into_iter().map(|(k, v)| (k, v.subst(&substs))).collect();
                        self.resolve(i2, s2)
                    }
                }
                _ => {
                    let tn = self.cg.typed.fmt_ty(&self_ty);
                    if f.decl.name.name == "to_string" {
                        return Target::Intrinsic("display_to_string".into());
                    }
                    Target::Missing(format!(
                        "no implementation of `{}` for `{tn}`",
                        f.decl.name.name
                    ))
                }
            };
        }
        if f.decl.body.is_none() {
            return Target::Intrinsic(crate::intrinsics::qualified_name(self.cg, idx));
        }
        Target::Direct(self.cg.instance(idx, 0, substs, Abi::Plain))
    }

    fn call_ret_ty(&mut self, callee: &Callee, args: &[Operand]) -> Ty {
        match callee {
            Callee::Fn(i, substs) => {
                let s: HashMap<String, Ty> = substs
                    .iter()
                    .map(|(k, v)| (k.clone(), self.subst(v)))
                    .collect();
                let (_, ret) = self.cg.typed.signature(*i);
                ret.subst(&s)
            }
            Callee::Value => match args.first().map(|a| self.op_ty(a)) {
                Some(Ty::Fn(_, r)) => *r,
                _ => Ty::Unit,
            },
            Callee::Intrinsic(_) => Ty::Unit,
        }
    }

    /// Copies each argument into a fresh slot (the callee owns it) and returns the slot addresses.
    fn arg_slots(&mut self, args: &[Operand]) -> Vec<Value> {
        args.iter()
            .map(|a| {
                let t = self.op_ty(a);
                let s = self.slot_for(&t);
                self.store_op(a, s);
                s
            })
            .collect()
    }

    fn call(&mut self, callee: &Callee, args: &[Operand], dest: Value, dt: &Ty) {
        match callee {
            Callee::Fn(i, substs) => {
                let s: HashMap<String, Ty> = substs
                    .iter()
                    .map(|(k, v)| (k.clone(), self.subst(v)))
                    .collect();
                match self.resolve(*i, s) {
                    Target::Direct(id) => {
                        let mut vals = vec![dest];
                        vals.extend(self.arg_slots(args));
                        self.call_fn(id, &vals);
                    }
                    Target::Virtual(k) => {
                        // `args[0]` is `&dyn Trait` / `&mut dyn Trait`: (data, vtable).
                        let a0 = self.op_addr(&args[0]);
                        let data = self.load(types::I64, a0, 0);
                        let vt = self.load(types::I64, a0, 8);
                        let code = self.load(types::I64, vt, 24 + 8 * k as i32);
                        let self_slot = self.slot(8, 8);
                        self.store(data, self_slot, 0);
                        let mut vals = vec![dest, self_slot];
                        vals.extend(self.arg_slots(&args[1..]));
                        let sig = self.cg.ptr_sig(vals.len());
                        let sref = self.b.import_signature(sig);
                        self.b.ins().call_indirect(sref, code, &vals);
                    }
                    Target::Intrinsic(name) => self.intrinsic(&name, args, dest, dt),
                    Target::Missing(m) => self.error(m),
                }
            }
            Callee::Value => {
                let fa = self.op_addr(&args[0]);
                let code = self.load(types::I64, fa, 0);
                let env = self.load(types::I64, fa, 8);
                let mut vals = vec![dest, env];
                vals.extend(self.arg_slots(&args[1..]));
                let sig = self.cg.ptr_sig(vals.len());
                let sref = self.b.import_signature(sig);
                self.b.ins().call_indirect(sref, code, &vals);
            }
            Callee::Intrinsic(name) => self.intrinsic(name, args, dest, dt),
        }
    }

    // ----- operators -----------------------------------------------------------------

    /// A scalar operand, looking through references (match ergonomics).
    pub(crate) fn scalar_through_refs(&mut self, o: &Operand) -> (Value, Ty) {
        let mut t = self.op_ty(o);
        let mut v = self.op_scalar(o);
        while let Ty::Ref(_, inner) = &t {
            let inner = (**inner).clone();
            match self.cg.scalar(&inner) {
                Some(st) if !self.cg.is_fat(&t) => {
                    v = self.load(st, v, 0);
                    t = inner;
                }
                _ => break,
            }
        }
        (v, t)
    }

    pub(crate) fn is_str_like(&self, t: &Ty) -> bool {
        match t {
            Ty::Prim(PrimTy::Str) => true,
            Ty::Ref(_, x) => self.is_str_like(x),
            _ => self.cg.is_lang(t, "String"),
        }
    }

    /// `(ptr, len)` of a string-like value stored at `addr` with type `t`.
    pub(crate) fn str_at(&mut self, addr: Value, t: &Ty) -> (Value, Value) {
        match t {
            Ty::Ref(_, x) if **x == Ty::Prim(PrimTy::Str) => (
                self.load(types::I64, addr, 0),
                self.load(types::I64, addr, 8),
            ),
            Ty::Ref(_, x) => {
                let p = self.load(types::I64, addr, 0);
                let x = (**x).clone();
                self.str_at(p, &x)
            }
            _ => (
                self.load(types::I64, addr, 0),
                self.load(types::I64, addr, 16),
            ),
        }
    }

    pub(crate) fn str_of(&mut self, o: &Operand) -> (Value, Value) {
        let t = self.op_ty(o);
        let a = self.op_addr(o);
        self.str_at(a, &t)
    }

    fn is_signed(t: &Ty) -> bool {
        matches!(
            t,
            Ty::Prim(
                PrimTy::I8 | PrimTy::I16 | PrimTy::I32 | PrimTy::I64 | PrimTy::I128 | PrimTy::Isize
            )
        )
    }

    fn pure(&mut self, op: &Op, ops: &[Operand], dest: Value, dt: &Ty) {
        match op {
            Op::Bin(b) => {
                let v = self.binary(*b, &ops[0], &ops[1]);
                if let Some(v) = v {
                    self.store(v, dest, 0);
                }
            }
            Op::Neg => {
                let (v, t) = self.scalar_through_refs(&ops[0]);
                let r = if matches!(t, Ty::Prim(PrimTy::F32 | PrimTy::F64)) {
                    self.b.ins().fneg(v)
                } else if self.cg.checked && Self::is_signed(&t) && t != Ty::Prim(PrimTy::I128) {
                    let it = self.cg.scalar(&t).unwrap();
                    let z = self.b.ins().iconst(it, 0);
                    self.checked_arith(BinOpKind::Sub, z, v, it, true)
                } else {
                    self.b.ins().ineg(v)
                };
                self.store(r, dest, 0);
            }
            Op::Not => {
                let (v, t) = self.scalar_through_refs(&ops[0]);
                let r = if t == Ty::Prim(PrimTy::Bool) {
                    self.b.ins().icmp_imm(IntCC::Equal, v, 0)
                } else {
                    self.b.ins().bnot(v)
                };
                self.store(r, dest, 0);
            }
            Op::Cast(to) => {
                let to = self.subst(to);
                let (v, from) = self.scalar_through_refs(&ops[0]);
                let r = self.cast(v, &from, &to);
                self.store(r, dest, 0);
            }
            Op::Discriminant => {
                let a = self.op_addr(&ops[0]);
                let tag = self.load(types::I32, a, 0);
                let t = self.b.ins().uextend(types::I64, tag);
                self.store(t, dest, 0);
            }
            Op::Print { newline, stderr } => {
                let t = self.op_ty(&ops[0]);
                let nl = self.b.ins().iconst(types::I32, *newline as i64);
                let se = self.b.ins().iconst(types::I32, *stderr as i64);
                if self.is_str_like(&t) {
                    let (p, n) = self.str_of(&ops[0]);
                    self.call_rt("kn_print", &[p, n, se, nl]);
                } else {
                    let s = self.new_string();
                    let a = self.op_addr(&ops[0]);
                    self.fmt_into(s, a, &t);
                    let p = self.load(types::I64, s, 0);
                    let n = self.load(types::I64, s, 16);
                    self.call_rt("kn_print", &[p, n, se, nl]);
                    self.free_string(s);
                }
            }
            Op::Interp(segs) => {
                self.init_string(dest);
                for seg in segs {
                    match seg {
                        InterpSeg::Text(t) => {
                            let (p, n) = self.str_lit(t);
                            self.call_rt("kn_string_push", &[dest, p, n]);
                        }
                        InterpSeg::Arg(i) => {
                            let t = self.op_ty(&ops[*i]);
                            let a = self.op_addr(&ops[*i]);
                            self.fmt_into(dest, a, &t);
                        }
                    }
                }
                let _ = dt;
            }
            Op::Touch => {}
            Op::Unsupported(what) => self.error(format!("code generation does not support {what}")),
        }
    }

    pub(crate) fn init_string(&mut self, s: Value) {
        let one = self.iconst(1);
        let z = self.iconst(0);
        self.store(one, s, 0);
        self.store(z, s, 8);
        self.store(z, s, 16);
    }

    pub(crate) fn new_string(&mut self) -> Value {
        let s = self.slot(24, 8);
        self.init_string(s);
        s
    }

    pub(crate) fn free_string(&mut self, s: Value) {
        let one = self.iconst(1);
        self.call_rt("kn_vec_free", &[s, one, one]);
    }

    /// Appends the `Display` form of the value at `addr` (type `t`) to String `s`.
    pub(crate) fn fmt_into(&mut self, s: Value, addr: Value, t: &Ty) {
        if self.is_str_like(t) {
            let (p, n) = self.str_at(addr, t);
            self.call_rt("kn_string_push", &[s, p, n]);
            return;
        }
        match t {
            Ty::Ref(..) | Ty::Adt(..) if self.cg.is_lang(t, "Box") || matches!(t, Ty::Ref(..)) => {
                let inner = match t {
                    Ty::Ref(_, x) => (**x).clone(),
                    Ty::Adt(_, a) => a[0].clone(),
                    _ => unreachable!(),
                };
                let p = self.load(types::I64, addr, 0);
                let _ = &inner;
                self.fmt_into(s, p, &inner)
            }
            Ty::Prim(p) => {
                let st = self.cg.scalar(t).unwrap_or(types::I64);
                let v = self.load(st, addr, 0);
                match p {
                    PrimTy::Bool => {
                        let w = self.b.ins().uextend(types::I32, v);
                        self.call_rt("kn_fmt_bool", &[s, w]);
                    }
                    PrimTy::Char => {
                        self.call_rt("kn_fmt_char", &[s, v]);
                    }
                    PrimTy::F32 => {
                        let w = self.b.ins().fpromote(types::F64, v);
                        self.call_rt("kn_fmt_f64", &[s, w]);
                    }
                    PrimTy::F64 => {
                        self.call_rt("kn_fmt_f64", &[s, v]);
                    }
                    _ if Self::is_signed(t) => {
                        let w = if st.bits() < 64 {
                            self.b.ins().sextend(types::I64, v)
                        } else {
                            v
                        };
                        self.call_rt("kn_fmt_i64", &[s, w]);
                    }
                    _ => {
                        let w = if st.bits() < 64 {
                            self.b.ins().uextend(types::I64, v)
                        } else {
                            v
                        };
                        self.call_rt("kn_fmt_u64", &[s, w]);
                    }
                }
            }
            Ty::Adt(..) => {
                // A user type implementing `Display`: call its `to_string`.
                let Some(disp) = self.cg.r.prelude_type("Display") else {
                    return;
                };
                let Some(&m) = self.cg.typed.trait_methods(disp).first() else {
                    return self.error("`Display` has no `to_string` method");
                };
                let mut substs = HashMap::new();
                substs.insert("Self".to_string(), t.clone());
                match self.resolve(m, substs) {
                    Target::Direct(id) => {
                        let tmp = self.slot(24, 8);
                        let r = self.slot(8, 8);
                        self.store(addr, r, 0);
                        self.call_fn(id, &[tmp, r]);
                        let p = self.load(types::I64, tmp, 0);
                        let n = self.load(types::I64, tmp, 16);
                        self.call_rt("kn_string_push", &[s, p, n]);
                        self.free_string(tmp);
                    }
                    _ => {
                        let n = self.cg.typed.fmt_ty(t);
                        self.error(format!("cannot format a value of type `{n}`: implement `Display` (with `fn to_string(&self) -> String`)"))
                    }
                }
            }
            Ty::Unit => {
                let (p, n) = self.str_lit("()");
                self.call_rt("kn_string_push", &[s, p, n]);
            }
            _ => {
                let n = self.cg.typed.fmt_ty(t);
                self.error(format!(
                    "code generation cannot format a value of type `{n}` yet"
                ))
            }
        }
    }

    fn cast(&mut self, v: Value, from: &Ty, to: &Ty) -> Value {
        let (Some(fs), Some(ts)) = (self.cg.scalar(from), self.cg.scalar(to)) else {
            self.error("unsupported cast");
            return v;
        };
        let ff = fs.is_float();
        let tf = ts.is_float();
        match (ff, tf) {
            (false, false) => {
                if fs.bits() == ts.bits() {
                    v
                } else if fs.bits() > ts.bits() {
                    self.b.ins().ireduce(ts, v)
                } else if Self::is_signed(from) {
                    self.b.ins().sextend(ts, v)
                } else {
                    self.b.ins().uextend(ts, v)
                }
            }
            (false, true) => {
                if Self::is_signed(from) {
                    self.b.ins().fcvt_from_sint(ts, v)
                } else {
                    self.b.ins().fcvt_from_uint(ts, v)
                }
            }
            (true, false) => {
                if Self::is_signed(to) {
                    self.b.ins().fcvt_to_sint_sat(ts, v)
                } else {
                    self.b.ins().fcvt_to_uint_sat(ts, v)
                }
            }
            (true, true) => match fs.bits().cmp(&ts.bits()) {
                std::cmp::Ordering::Less => self.b.ins().fpromote(ts, v),
                std::cmp::Ordering::Greater => self.b.ins().fdemote(ts, v),
                std::cmp::Ordering::Equal => v,
            },
        }
    }

    /// `a op b` for `+`, `-`, `*` on integers of type `it`, panicking on overflow
    /// (debug builds).
    fn checked_arith(
        &mut self,
        op: BinOpKind,
        a: Value,
        b: Value,
        it: ir::Type,
        signed: bool,
    ) -> Value {
        use BinOpKind::*;
        let what = match op {
            Add => "add",
            Sub => "subtract",
            _ => "multiply",
        };
        let msg = format!("attempt to {what} with overflow");
        let (res, overflow) = if it.bits() < 64 {
            // Compute exactly in 64 bits, then check the result fits.
            let ext = |cx: &mut Self, v| {
                if signed {
                    cx.b.ins().sextend(types::I64, v)
                } else {
                    cx.b.ins().uextend(types::I64, v)
                }
            };
            let (x, y) = (ext(self, a), ext(self, b));
            let wide = match op {
                Add => self.b.ins().iadd(x, y),
                Sub => self.b.ins().isub(x, y),
                _ => self.b.ins().imul(x, y),
            };
            let res = self.b.ins().ireduce(it, wide);
            let back = ext(self, res);
            let ov = self.b.ins().icmp(IntCC::NotEqual, back, wide);
            (res, ov)
        } else {
            match (op, signed) {
                (Add, false) => {
                    let r = self.b.ins().iadd(a, b);
                    (r, self.b.ins().icmp(IntCC::UnsignedLessThan, r, a))
                }
                (Sub, false) => {
                    let r = self.b.ins().isub(a, b);
                    (r, self.b.ins().icmp(IntCC::UnsignedLessThan, a, b))
                }
                (Add, true) => {
                    // Overflow iff both operands have the sign opposite to the result.
                    let r = self.b.ins().iadd(a, b);
                    let x = self.b.ins().bxor(a, r);
                    let y = self.b.ins().bxor(b, r);
                    let m = self.b.ins().band(x, y);
                    (r, self.b.ins().icmp_imm(IntCC::SignedLessThan, m, 0))
                }
                (Sub, true) => {
                    let r = self.b.ins().isub(a, b);
                    let x = self.b.ins().bxor(a, b);
                    let y = self.b.ins().bxor(a, r);
                    let m = self.b.ins().band(x, y);
                    (r, self.b.ins().icmp_imm(IntCC::SignedLessThan, m, 0))
                }
                (_, false) => {
                    let r = self.b.ins().imul(a, b);
                    let hi = self.b.ins().umulhi(a, b);
                    (r, self.b.ins().icmp_imm(IntCC::NotEqual, hi, 0))
                }
                (_, true) => {
                    let r = self.b.ins().imul(a, b);
                    let hi = self.b.ins().smulhi(a, b);
                    let sign = self.b.ins().sshr_imm(r, 63);
                    (r, self.b.ins().icmp(IntCC::NotEqual, hi, sign))
                }
            }
        };
        self.when(overflow, |cx| cx.panic_msg(&msg));
        res
    }

    fn binary(&mut self, op: BinOpKind, l: &Operand, r: &Operand) -> Option<Value> {
        use BinOpKind::*;
        let lt = self.op_ty(l);
        if self.is_str_like(&lt) && !matches!(op, Add) {
            let (a, an) = self.str_of(l);
            let (b, bn) = self.str_of(r);
            let res = match op {
                Eq | Ne => {
                    let e = self.call_rt("kn_str_eq", &[a, an, b, bn]).unwrap();
                    self.b.ins().icmp_imm(
                        if op == Eq {
                            IntCC::NotEqual
                        } else {
                            IntCC::Equal
                        },
                        e,
                        0,
                    )
                }
                _ => {
                    let c = self.call_rt("kn_str_cmp", &[a, an, b, bn]).unwrap();
                    let cc = match op {
                        Lt => IntCC::SignedLessThan,
                        Le => IntCC::SignedLessThanOrEqual,
                        Gt => IntCC::SignedGreaterThan,
                        _ => IntCC::SignedGreaterThanOrEqual,
                    };
                    self.b.ins().icmp_imm(cc, c, 0)
                }
            };
            return Some(res);
        }
        let (a, t) = self.scalar_through_refs(l);
        let (b, _) = self.scalar_through_refs(r);
        if t == Ty::Unit {
            return Some(
                self.b
                    .ins()
                    .iconst(types::I8, matches!(op, Eq | Le | Ge) as i64),
            );
        }
        if self.cg.scalar(&t).is_none() {
            let n = self.cg.typed.fmt_ty(&t);
            self.error(format!(
                "code generation does not support comparing or combining values of type `{n}` yet"
            ));
            return None;
        }
        let ins = self.b.ins();
        let _ = ins;
        if matches!(t, Ty::Prim(PrimTy::F32 | PrimTy::F64)) {
            let cc = |c| Some(c);
            let fcmp = |s: &mut Self, c: FloatCC| Some(s.b.ins().fcmp(c, a, b));
            return match op {
                Add => cc(self.b.ins().fadd(a, b)),
                Sub => cc(self.b.ins().fsub(a, b)),
                Mul => cc(self.b.ins().fmul(a, b)),
                Div => cc(self.b.ins().fdiv(a, b)),
                Rem => {
                    let ft = self.cg.scalar(&t).unwrap();
                    if ft == types::F64 {
                        self.call_rt("kn_fmod", &[a, b])
                    } else {
                        let x = self.b.ins().fpromote(types::F64, a);
                        let y = self.b.ins().fpromote(types::F64, b);
                        let m = self.call_rt("kn_fmod", &[x, y]).unwrap();
                        Some(self.b.ins().fdemote(types::F32, m))
                    }
                }
                Eq => fcmp(self, FloatCC::Equal),
                Ne => fcmp(self, FloatCC::NotEqual),
                Lt => fcmp(self, FloatCC::LessThan),
                Le => fcmp(self, FloatCC::LessThanOrEqual),
                Gt => fcmp(self, FloatCC::GreaterThan),
                Ge => fcmp(self, FloatCC::GreaterThanOrEqual),
                _ => {
                    self.error("invalid float operation");
                    None
                }
            };
        }
        let signed = Self::is_signed(&t);
        let icmp = |s: &mut Self, sc: IntCC, uc: IntCC| {
            Some(s.b.ins().icmp(if signed { sc } else { uc }, a, b))
        };
        let it = self.cg.scalar(&t).unwrap();
        match op {
            Add | Sub | Mul if self.cg.checked && it != types::I128 => {
                Some(self.checked_arith(op, a, b, it, signed))
            }
            Add => Some(self.b.ins().iadd(a, b)),
            Sub => Some(self.b.ins().isub(a, b)),
            Mul => Some(self.b.ins().imul(a, b)),
            Div | Rem => {
                let zero = self.b.ins().icmp_imm(IntCC::Equal, b, 0);
                let msg = if op == Div {
                    "attempt to divide by zero"
                } else {
                    "attempt to calculate the remainder with a divisor of zero"
                };
                self.when(zero, |cx| cx.panic_msg(msg));
                if signed {
                    // MIN / -1 overflows (and traps in hardware): panic in all builds.
                    let min = match it.bits() {
                        8 => i8::MIN as i64,
                        16 => i16::MIN as i64,
                        32 => i32::MIN as i64,
                        _ => i64::MIN,
                    };
                    if it != types::I128 {
                        let neg1 = self.b.ins().icmp_imm(IntCC::Equal, b, -1);
                        let is_min = self.b.ins().icmp_imm(IntCC::Equal, a, min);
                        let both = self.b.ins().band(neg1, is_min);
                        let msg = if op == Div {
                            "attempt to divide with overflow"
                        } else {
                            "attempt to calculate the remainder with overflow"
                        };
                        self.when(both, |cx| cx.panic_msg(msg));
                    }
                }
                Some(match (op, signed) {
                    (Div, true) => self.b.ins().sdiv(a, b),
                    (Div, false) => self.b.ins().udiv(a, b),
                    (_, true) => self.b.ins().srem(a, b),
                    _ => self.b.ins().urem(a, b),
                })
            }
            BitAnd | And => Some(self.b.ins().band(a, b)),
            BitOr | Or => Some(self.b.ins().bor(a, b)),
            BitXor => Some(self.b.ins().bxor(a, b)),
            Shl => Some(self.b.ins().ishl(a, b)),
            Shr => Some(if signed {
                self.b.ins().sshr(a, b)
            } else {
                self.b.ins().ushr(a, b)
            }),
            Eq => Some(self.b.ins().icmp(IntCC::Equal, a, b)),
            Ne => Some(self.b.ins().icmp(IntCC::NotEqual, a, b)),
            Lt => icmp(self, IntCC::SignedLessThan, IntCC::UnsignedLessThan),
            Le => icmp(
                self,
                IntCC::SignedLessThanOrEqual,
                IntCC::UnsignedLessThanOrEqual,
            ),
            Gt => icmp(self, IntCC::SignedGreaterThan, IntCC::UnsignedGreaterThan),
            Ge => icmp(
                self,
                IntCC::SignedGreaterThanOrEqual,
                IntCC::UnsignedGreaterThanOrEqual,
            ),
        }
    }
}
