//! Cranelift IR → LLVM IR.

use crate::{ptrs, Func, Settings};
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, ExternalName, GlobalValueData,
    InstructionData as D, Opcode as O, Value,
};
use cranelift_module::{
    DataDescription, DataId, FuncId, Init, Linkage, ModuleDeclarations, ModuleRelocTarget,
};
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::{Linkage as L, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, GlobalValue,
    InstructionValue, IntValue, PhiValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};
use std::collections::{HashMap, HashSet};

type R<T> = Result<T, String>;

fn e<E: std::fmt::Debug>(x: E) -> String {
    format!("LLVM builder error: {x:?}")
}

struct Cx<'c> {
    c: &'c Context,
    m: Module<'c>,
    funcs: HashMap<FuncId, FunctionValue<'c>>,
    data: HashMap<DataId, GlobalValue<'c>>,
}

impl<'c> Cx<'c> {
    fn ptr_t(&self) -> inkwell::types::PointerType<'c> {
        self.c.ptr_type(AddressSpace::default())
    }

    fn ty(&self, t: ir::Type) -> R<BasicTypeEnum<'c>> {
        Ok(match t {
            types::I8 => self.c.i8_type().into(),
            types::I16 => self.c.i16_type().into(),
            types::I32 => self.c.i32_type().into(),
            types::I64 => self.c.i64_type().into(),
            types::I128 => self.c.i128_type().into(),
            types::F32 => self.c.f32_type().into(),
            types::F64 => self.c.f64_type().into(),
            t => return Err(format!("LLVM backend: unsupported type {t}")),
        })
    }

    /// The LLVM type of a Cranelift signature. Kiln functions (and indirect
    /// calls, which always reach Kiln functions or shims) take their `i64`
    /// parameters as `ptr`: every one of them is an address.
    fn fn_type(&self, sig: &ir::Signature, ptr_params: bool) -> R<FunctionType<'c>> {
        let mut ps: Vec<BasicMetadataTypeEnum> = Vec::new();
        for p in &sig.params {
            if ptr_params && p.value_type == types::I64 {
                ps.push(self.ptr_t().into());
            } else {
                ps.push(self.ty(p.value_type)?.into());
            }
        }
        Ok(match sig.returns.len() {
            0 => self.c.void_type().fn_type(&ps, false),
            1 => self.ty(sig.returns[0].value_type)?.fn_type(&ps, false),
            _ => {
                let rs: Vec<BasicTypeEnum> = sig
                    .returns
                    .iter()
                    .map(|r| self.ty(r.value_type))
                    .collect::<R<_>>()?;
                self.c.struct_type(&rs, false).fn_type(&ps, false)
            }
        })
    }

    fn enum_attr(&self, name: &str) -> Attribute {
        self.c
            .create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0)
    }

    fn intrinsic(&self, name: &str, tys: &[BasicTypeEnum<'c>]) -> R<FunctionValue<'c>> {
        Intrinsic::find(name)
            .and_then(|i| i.get_declaration(&self.m, tys))
            .ok_or_else(|| format!("LLVM backend: missing intrinsic {name}"))
    }
}

fn linkage(l: Linkage) -> L {
    match l {
        Linkage::Local => L::Internal,
        _ => L::External,
    }
}

pub fn emit(
    decls: &ModuleDeclarations,
    funcs: &[Func],
    data: &[(DataId, DataDescription)],
    settings: &Settings,
) -> R<Vec<u8>> {
    Target::initialize_native(&InitializationConfig::default()).map_err(|x| x.to_string())?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|x| x.to_string())?;
    let (cpu, features) = match settings.cpu.as_deref() {
        Some("native") => (
            TargetMachine::get_host_cpu_name().to_string(),
            TargetMachine::get_host_cpu_features().to_string(),
        ),
        Some(c) => (c.to_string(), String::new()),
        None if triple.as_str().to_string_lossy().starts_with("x86_64") => {
            ("x86-64".into(), String::new())
        }
        None => ("generic".into(), String::new()),
    };
    let tm = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::Aggressive,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("LLVM backend: no target machine for this host")?;
    let c = Context::create();
    let m = c.create_module("kiln");
    m.set_triple(&triple);
    m.set_data_layout(&tm.get_target_data().get_data_layout());
    let mut cx = Cx {
        c: &c,
        m,
        funcs: HashMap::new(),
        data: HashMap::new(),
    };

    let defined: HashMap<FuncId, &Func> = funcs.iter().map(|f| (f.id, f)).collect();
    for (id, d) in decls.get_functions() {
        let is_kiln = defined.contains_key(&id);
        let ty = cx.fn_type(&d.signature, is_kiln)?;
        let name = d.linkage_name(id);
        let fv = match cx.m.get_function(&name) {
            Some(f) => f,
            None => cx.m.add_function(
                &name,
                ty,
                Some(if is_kiln {
                    linkage(d.linkage)
                } else {
                    L::External
                }),
            ),
        };
        if name.starts_with("kn_panic") {
            fv.add_attribute(AttributeLoc::Function, cx.enum_attr("noreturn"));
            fv.add_attribute(AttributeLoc::Function, cx.enum_attr("cold"));
        }
        cx.funcs.insert(id, fv);
    }
    define_data(&mut cx, decls, data)?;
    let taken = address_taken(funcs, data);
    for f in funcs {
        let fv = cx.funcs[&f.id];
        let name = decls.get_function_decl(f.id).linkage_name(f.id).to_string();
        let local = decls.get_function_decl(f.id).linkage == Linkage::Local;
        let outer = match promoted_ret(&cx, f, local && !taken.contains(&f.id)) {
            Some(rt) => {
                ret_wrapper(&cx, fv, &name, f, rt).map_err(|x| format!("{x} (in `{name}`)"))?
            }
            None => fv,
        };
        let refs = valid_refs(&f.func, &f.hints);
        let target = if refs.is_empty() || std::env::var_os("KILN_LLVM_NO_REFS").is_some() {
            outer
        } else {
            thunk(&cx, outer, &name, f, &refs).map_err(|x| format!("{x} (in `{name}`)"))?
        };
        let refs = if target == outer { vec![] } else { refs };
        Fx::new(&cx, f, target, refs)?
            .run()
            .map_err(|x| format!("{x} (in `{name}`)"))?;
    }
    if std::env::var_os("KILN_LLVM_VERIFY").is_some() {
        cx.m.verify()
            .map_err(|x| format!("LLVM verifier: {}", x.to_string()))?;
    }
    cx.m.run_passes("default<O3>", &tm, PassBuilderOptions::create())
        .map_err(|x| format!("LLVM optimization failed: {}", x.to_string()))?;
    if let Some(p) = &settings.dump_ir {
        cx.m.print_to_file(p).map_err(|x| x.to_string())?;
    }
    let buf = tm
        .write_to_memory_buffer(&cx.m, FileType::Object)
        .map_err(|x| format!("LLVM code generation failed: {}", x.to_string()))?;
    Ok(buf.as_slice().to_vec())
}

enum Reloc {
    Func(FuncId),
    Data(DataId, i64),
}

fn reloc_target(t: &ModuleRelocTarget) -> R<(u32, u32)> {
    match t {
        ModuleRelocTarget::User { namespace, index } => Ok((*namespace, *index)),
        t => Err(format!("LLVM backend: unsupported data relocation {t:?}")),
    }
}

fn define_data(
    cx: &mut Cx<'_>,
    decls: &ModuleDeclarations,
    data: &[(DataId, DataDescription)],
) -> R<()> {
    let i8t = cx.c.i8_type();
    let defined: HashSet<DataId> = data.iter().map(|d| d.0).collect();
    // Declarations first: data may point at other data.
    let mut layouts = HashMap::new();
    for (id, desc) in data {
        let d = decls.get_data_decl(*id);
        let size = match &desc.init {
            Init::Zeros { size } => *size,
            Init::Bytes { contents } => contents.len(),
            Init::Uninitialized => return Err("LLVM backend: uninitialized data".into()),
        };
        let mut relocs: Vec<(usize, Reloc)> = Vec::new();
        for (off, fr) in &desc.function_relocs {
            let (_, idx) = reloc_target(&desc.function_decls[*fr])?;
            relocs.push((*off as usize, Reloc::Func(FuncId::from_u32(idx))));
        }
        for (off, gv, add) in &desc.data_relocs {
            let (ns, idx) = reloc_target(&desc.data_decls[*gv])?;
            if ns == 0 {
                relocs.push((*off as usize, Reloc::Func(FuncId::from_u32(idx))));
            } else {
                relocs.push((*off as usize, Reloc::Data(DataId::from_u32(idx), *add)));
            }
        }
        relocs.sort_by_key(|r| r.0);
        let mut fields: Vec<BasicTypeEnum> = Vec::new();
        let mut pos = 0;
        for (off, _) in &relocs {
            if *off > pos {
                fields.push(i8t.array_type((*off - pos) as u32).into());
            }
            fields.push(cx.ptr_t().into());
            pos = off + 8;
        }
        if size > pos {
            fields.push(i8t.array_type((size - pos) as u32).into());
        }
        let ty = cx.c.struct_type(&fields, true);
        let name = d.linkage_name(*id);
        let g = cx.m.add_global(ty, None, &name);
        g.set_linkage(linkage(d.linkage));
        g.set_constant(!d.writable);
        let align = desc.align.unwrap_or(1).max(if size >= 8 { 8 } else { 1 });
        g.set_alignment(align as u32);
        cx.data.insert(*id, g);
        layouts.insert(*id, (size, relocs));
    }
    for (id, d) in decls.get_data_objects() {
        if !defined.contains(&id) {
            let g = cx.m.add_global(i8t, None, &d.linkage_name(id));
            g.set_linkage(L::External);
            cx.data.insert(id, g);
        }
    }
    for (id, desc) in data {
        let (size, relocs) = &layouts[id];
        let bytes: Vec<u8> = match &desc.init {
            Init::Bytes { contents } => contents.to_vec(),
            _ => vec![0; *size],
        };
        let arr = |a: usize, b: usize| -> BasicValueEnum {
            let vs: Vec<IntValue> = bytes[a..b]
                .iter()
                .map(|&x| i8t.const_int(x as u64, false))
                .collect();
            i8t.const_array(&vs).into()
        };
        let mut vals: Vec<BasicValueEnum> = Vec::new();
        let mut pos = 0;
        for (off, r) in relocs {
            if *off > pos {
                vals.push(arr(pos, *off));
            }
            let p = match r {
                Reloc::Func(f) => cx.funcs[f].as_global_value().as_pointer_value(),
                Reloc::Data(d, add) => {
                    let g = cx.data[d].as_pointer_value();
                    if *add == 0 {
                        g
                    } else {
                        unsafe { g.const_gep(i8t, &[cx.c.i64_type().const_int(*add as u64, true)]) }
                    }
                }
            };
            vals.push(p.into());
            pos = off + 8;
        }
        if *size > pos {
            vals.push(arr(pos, *size));
        }
        cx.data[id].set_initializer(&cx.c.const_struct(&vals, true));
    }
    Ok(())
}

/// One function being translated.
struct Fx<'a, 'c> {
    cx: &'a Cx<'c>,
    f: &'a ir::Function,
    hints: &'a crate::FnHints,
    fv: FunctionValue<'c>,
    b: Builder<'c>,
    vals: HashMap<Value, BasicValueEnum<'c>>,
    ptrs: HashSet<Value>,
    blocks: HashMap<ir::Block, BasicBlock<'c>>,
    phis: HashMap<Value, PhiValue<'c>>,
    slots: HashMap<ir::StackSlot, PointerValue<'c>>,
    trap_bb: Option<BasicBlock<'c>>,
    /// Reference arguments passed separately to the body (see [`crate::RefHint`]): slot parameter → value.
    refvals: HashMap<usize, BasicValueEnum<'c>>,
    refs: Vec<crate::RefHint>,
    /// Return-slot promotion: the result is built in this local and
    /// returned by value (see [`promoted_ret`]).
    ret_ty: Option<BasicTypeEnum<'c>>,
    ret_slot: Option<PointerValue<'c>>,
}

impl<'a, 'c> Fx<'a, 'c> {
    fn new(
        cx: &'a Cx<'c>,
        func: &'a Func,
        fv: FunctionValue<'c>,
        refs: Vec<crate::RefHint>,
    ) -> R<Self> {
        let f = &func.func;
        Ok(Fx {
            cx,
            f,
            hints: &func.hints,
            fv,
            b: cx.c.create_builder(),
            vals: HashMap::new(),
            ptrs: ptrs::infer(f, true),
            blocks: HashMap::new(),
            phis: HashMap::new(),
            slots: HashMap::new(),
            trap_bb: None,
            refvals: HashMap::new(),
            refs,
            ret_ty: fv
                .get_type()
                .get_return_type()
                .filter(|_| f.signature.returns.is_empty()),
            ret_slot: None,
        })
    }

    fn i64t(&self) -> inkwell::types::IntType<'c> {
        self.cx.c.i64_type()
    }

    /// The LLVM type a Cranelift value is kept in.
    fn vty(&self, v: Value) -> R<BasicTypeEnum<'c>> {
        let v = self.f.dfg.resolve_aliases(v);
        let t = self.f.dfg.value_type(v);
        if t == types::I64 && self.ptrs.contains(&v) {
            Ok(self.cx.ptr_t().into())
        } else {
            self.cx.ty(t)
        }
    }

    fn get(&self, v: Value) -> R<BasicValueEnum<'c>> {
        let v = self.f.dfg.resolve_aliases(v);
        self.vals.get(&v).copied().ok_or_else(|| {
            let def = match self.f.dfg.value_def(v) {
                ir::ValueDef::Result(i, _) => format!("`{}`", self.f.dfg.display_inst(i)),
                d => format!("{d:?}"),
            };
            format!("LLVM backend: value {v} used before definition (defined by {def})")
        })
    }

    fn int(&self, v: Value) -> R<IntValue<'c>> {
        Ok(match self.get(v)? {
            BasicValueEnum::PointerValue(p) => {
                self.b.build_ptr_to_int(p, self.i64t(), "").map_err(e)?
            }
            BasicValueEnum::IntValue(i) => i,
            BasicValueEnum::FloatValue(f) => {
                let it = self
                    .cx
                    .c
                    .custom_width_int_type(self.f.dfg.value_type(v).bits());
                self.b
                    .build_bit_cast(f, it, "")
                    .map_err(e)?
                    .into_int_value()
            }
            x => return Err(format!("LLVM backend: unexpected value {x:?}")),
        })
    }

    fn ptr(&self, v: Value) -> R<PointerValue<'c>> {
        Ok(match self.get(v)? {
            BasicValueEnum::PointerValue(p) => p,
            BasicValueEnum::IntValue(i) => {
                self.b.build_int_to_ptr(i, self.cx.ptr_t(), "").map_err(e)?
            }
            x => return Err(format!("LLVM backend: not an address: {x:?}")),
        })
    }

    fn flt(&self, v: Value) -> R<FloatValue<'c>> {
        match self.get(v)? {
            BasicValueEnum::FloatValue(f) => Ok(f),
            x => Err(format!("LLVM backend: not a float: {x:?}")),
        }
    }

    /// Converts `x` to the representation `ty`.
    fn coerce(&self, x: BasicValueEnum<'c>, ty: BasicTypeEnum<'c>) -> R<BasicValueEnum<'c>> {
        Ok(match (x, ty) {
            (BasicValueEnum::IntValue(i), BasicTypeEnum::PointerType(p)) => {
                self.b.build_int_to_ptr(i, p, "").map_err(e)?.into()
            }
            (BasicValueEnum::PointerValue(p), BasicTypeEnum::IntType(t)) => {
                self.b.build_ptr_to_int(p, t, "").map_err(e)?.into()
            }
            _ => x,
        })
    }

    fn set(&mut self, v: Value, x: BasicValueEnum<'c>) -> R<()> {
        let ty = self.vty(v)?;
        let x = self.coerce(x, ty)?;
        self.vals.insert(v, x);
        Ok(())
    }

    fn res(&self, inst: ir::Inst) -> Value {
        self.f.dfg.first_result(inst)
    }

    fn gep(&self, base: PointerValue<'c>, off: IntValue<'c>) -> R<PointerValue<'c>> {
        unsafe { self.b.build_gep(self.cx.c.i8_type(), base, &[off], "") }.map_err(e)
    }

    fn addr(&self, v: Value, off: i64) -> R<PointerValue<'c>> {
        let p = self.ptr(v)?;
        if off == 0 {
            Ok(p)
        } else {
            self.gep(p, self.i64t().const_int(off as u64, true))
        }
    }

    fn slot_addr(&self, ss: ir::StackSlot, off: i64) -> R<PointerValue<'c>> {
        let p = self.slots[&ss];
        if off == 0 {
            Ok(p)
        } else {
            self.gep(p, self.i64t().const_int(off as u64, true))
        }
    }

    fn load(
        &self,
        ty: BasicTypeEnum<'c>,
        p: PointerValue<'c>,
        aligned: bool,
    ) -> R<BasicValueEnum<'c>> {
        let x = self.b.build_load(ty, p, "").map_err(e)?;
        let inst = x.as_instruction_value().unwrap();
        let _ = inst.set_alignment(if aligned { natural(ty) } else { 1 });
        Ok(x)
    }

    fn store(&self, x: BasicValueEnum<'c>, p: PointerValue<'c>, aligned: bool) -> R<()> {
        let inst = self.b.build_store(p, x).map_err(e)?;
        let _ = inst.set_alignment(if aligned { natural(x.get_type()) } else { 1 });
        Ok(())
    }

    fn trap_block(&mut self) -> R<BasicBlock<'c>> {
        if let Some(b) = self.trap_bb {
            return Ok(b);
        }
        let cur = self.b.get_insert_block().unwrap();
        let bb = self.cx.c.append_basic_block(self.fv, "trap");
        self.b.position_at_end(bb);
        let t = self.cx.intrinsic("llvm.trap", &[])?;
        self.b.build_call(t, &[], "").map_err(e)?;
        self.b.build_unreachable().map_err(e)?;
        self.b.position_at_end(cur);
        self.trap_bb = Some(bb);
        Ok(bb)
    }

    fn run(mut self) -> R<()> {
        let f = self.f;
        let dfg = &f.dfg;
        let entry_bb = self.cx.c.append_basic_block(self.fv, "entry");
        for blk in f.layout.blocks() {
            let bb = self.cx.c.append_basic_block(self.fv, "");
            self.blocks.insert(blk, bb);
        }
        // Attributes from the Kiln types.
        self.fv
            .add_attribute(AttributeLoc::Function, self.cx.enum_attr("nounwind"));
        self.fv.add_attribute(
            AttributeLoc::Function,
            self.cx
                .c
                .create_string_attribute("probe-stack", "inline-asm"),
        );
        if self.hints.inline {
            self.fv
                .add_attribute(AttributeLoc::Function, self.cx.enum_attr("inlinehint"));
        }
        param_attrs(self.cx, self.fv, &self.hints.params);
        self.b.position_at_end(entry_bb);
        for (ss, d) in f.sized_stack_slots.iter() {
            let ty = self.cx.c.i8_type().array_type(d.size.max(1));
            let a = self.b.build_alloca(ty, "").map_err(e)?;
            let align = (1u32 << d.align_shift).max(8);
            let _ = a.as_instruction().unwrap().set_alignment(align);
            self.slots.insert(ss, a);
        }
        let entry = f
            .layout
            .entry_block()
            .ok_or("LLVM backend: empty function")?;
        let nparams = dfg.block_params(entry).len();
        if let Some(rt) = self.ret_ty {
            let a = self.b.build_alloca(rt, "ret").map_err(e)?;
            let _ = a.as_instruction().unwrap().set_alignment(8);
            self.ret_slot = Some(a);
        }
        for (i, &v) in dfg.block_params(entry).iter().enumerate() {
            let p = match (i, self.ret_slot) {
                (0, Some(a)) => a.into(),
                _ => self
                    .fv
                    .get_nth_param(i as u32)
                    .ok_or("LLVM backend: parameter count")?,
            };
            self.set(v, p)?;
        }
        for (j, r) in self.refs.iter().enumerate() {
            let p = self
                .fv
                .get_nth_param((nparams + j) as u32)
                .ok_or("LLVM backend: reference parameter")?;
            self.refvals.insert(r.param, p);
        }
        self.b
            .build_unconditional_branch(self.blocks[&entry])
            .map_err(e)?;
        for blk in f.layout.blocks() {
            if blk == entry {
                continue;
            }
            self.b.position_at_end(self.blocks[&blk]);
            for &v in dfg.block_params(blk) {
                let phi = self.b.build_phi(self.vty(v)?, "").map_err(e)?;
                self.phis.insert(v, phi);
                self.vals.insert(v, phi.as_basic_value());
            }
        }
        // Reverse postorder: definitions are translated before their uses.
        let mut seen = HashSet::new();
        let mut post = Vec::new();
        let mut stack = vec![(entry, false)];
        while let Some((b, done)) = stack.pop() {
            if done {
                post.push(b);
                continue;
            }
            if !seen.insert(b) {
                continue;
            }
            stack.push((b, true));
            if let Some(last) = f.layout.last_inst(b) {
                for (s, _) in ptrs::branch_args(f, last).into_iter().rev() {
                    if !seen.contains(&s) {
                        stack.push((s, false));
                    }
                }
            }
        }
        for &blk in post.iter().rev() {
            self.b.position_at_end(self.blocks[&blk]);
            for inst in f.layout.block_insts(blk) {
                self.inst(inst)?;
            }
        }
        for blk in f.layout.blocks() {
            if !seen.contains(&blk) {
                let bb = self.blocks[&blk];
                self.b.position_at_end(bb);
                self.b.build_unreachable().map_err(e)?;
            }
        }
        Ok(())
    }

    /// Adds the branch arguments for an edge from the current block.
    fn edge(&mut self, bc: &ir::BlockCall) -> R<BasicBlock<'c>> {
        let pool = &self.f.dfg.value_lists;
        let dest = bc.block(pool);
        let args = bc.args_slice(pool).to_vec();
        let from = self.b.get_insert_block().unwrap();
        let params = self.f.dfg.block_params(dest).to_vec();
        for (a, p) in args.iter().zip(params) {
            let phi = self.phis[&p];
            let x = self.coerce(self.get(*a)?, phi.as_basic_value().get_type())?;
            phi.add_incoming(&[(&x, from)]);
        }
        Ok(self.blocks[&dest])
    }

    /// An edge through its own block when the destination has parameters
    /// (so two edges from one block to one destination can pass different
    /// values).
    fn split_edge(&mut self, bc: &ir::BlockCall) -> R<BasicBlock<'c>> {
        let pool = &self.f.dfg.value_lists;
        if bc.args_slice(pool).is_empty() {
            return Ok(self.blocks[&bc.block(pool)]);
        }
        let cur = self.b.get_insert_block().unwrap();
        let mid = self.cx.c.insert_basic_block_after(cur, "");
        self.b.position_at_end(mid);
        let dest = self.edge(bc)?;
        self.b.build_unconditional_branch(dest).map_err(e)?;
        self.b.position_at_end(cur);
        Ok(mid)
    }

    fn int_ty(&self, v: Value) -> R<inkwell::types::IntType<'c>> {
        Ok(self.cx.ty(self.f.dfg.value_type(v))?.into_int_type())
    }

    fn cast_int(&self, x: IntValue<'c>, to: inkwell::types::IntType<'c>) -> R<IntValue<'c>> {
        let (a, b) = (x.get_type().get_bit_width(), to.get_bit_width());
        Ok(if a == b {
            x
        } else if a < b {
            self.b.build_int_z_extend(x, to, "").map_err(e)?
        } else {
            self.b.build_int_truncate(x, to, "").map_err(e)?
        })
    }

    fn bool8(&self, c: IntValue<'c>) -> R<BasicValueEnum<'c>> {
        Ok(self
            .b
            .build_int_z_extend(c, self.cx.c.i8_type(), "")
            .map_err(e)?
            .into())
    }

    fn nonzero(&self, v: Value) -> R<IntValue<'c>> {
        let x = self.int(v)?;
        if x.get_type().get_bit_width() == 1 {
            return Ok(x);
        }
        self.b
            .build_int_compare(IntPredicate::NE, x, x.get_type().const_zero(), "")
            .map_err(e)
    }

    fn call_intr(
        &self,
        name: &str,
        tys: &[BasicTypeEnum<'c>],
        args: &[BasicMetadataValueEnum<'c>],
    ) -> R<BasicValueEnum<'c>> {
        let f = self.cx.intrinsic(name, tys)?;
        self.b
            .build_call(f, args, "")
            .map_err(e)?
            .try_as_basic_value()
            .left()
            .ok_or_else(|| format!("{name} returned nothing"))
    }

    fn binary(&self, op: O, a: IntValue<'c>, b: IntValue<'c>) -> R<IntValue<'c>> {
        let bld = &self.b;
        let t = a.get_type();
        let w = t.get_bit_width();
        let shamt = |b: IntValue<'c>| -> R<IntValue<'c>> {
            let b = self.cast_int(b, t)?;
            bld.build_and(b, t.const_int((w - 1) as u64, false), "")
                .map_err(e)
        };
        match op {
            O::Iadd | O::IaddImm => bld.build_int_add(a, b, ""),
            O::Isub => bld.build_int_sub(a, b, ""),
            O::IrsubImm => bld.build_int_sub(b, a, ""),
            O::Imul | O::ImulImm => bld.build_int_mul(a, b, ""),
            O::Udiv | O::UdivImm => bld.build_int_unsigned_div(a, b, ""),
            O::Sdiv | O::SdivImm => bld.build_int_signed_div(a, b, ""),
            O::Urem | O::UremImm => bld.build_int_unsigned_rem(a, b, ""),
            O::Srem | O::SremImm => bld.build_int_signed_rem(a, b, ""),
            O::Band | O::BandImm => bld.build_and(a, b, ""),
            O::Bor | O::BorImm => bld.build_or(a, b, ""),
            O::Bxor | O::BxorImm => bld.build_xor(a, b, ""),
            O::Ishl | O::IshlImm => bld.build_left_shift(a, shamt(b)?, ""),
            O::Ushr | O::UshrImm => bld.build_right_shift(a, shamt(b)?, false, ""),
            O::Sshr | O::SshrImm => bld.build_right_shift(a, shamt(b)?, true, ""),
            O::Rotl | O::RotlImm | O::Rotr | O::RotrImm => {
                let n = if matches!(op, O::Rotl | O::RotlImm) {
                    "llvm.fshl"
                } else {
                    "llvm.fshr"
                };
                let b = self.cast_int(b, t)?;
                return Ok(self
                    .call_intr(n, &[t.into()], &[a.into(), a.into(), b.into()])?
                    .into_int_value());
            }
            O::Umulhi | O::Smulhi => {
                let wide = self.cx.c.custom_width_int_type(w * 2);
                let ext = |x: IntValue<'c>| {
                    if op == O::Umulhi {
                        bld.build_int_z_extend(x, wide, "")
                    } else {
                        bld.build_int_s_extend(x, wide, "")
                    }
                };
                let p = bld
                    .build_int_mul(ext(a).map_err(e)?, ext(b).map_err(e)?, "")
                    .map_err(e)?;
                let hi = bld
                    .build_right_shift(p, wide.const_int(w as u64, false), false, "")
                    .map_err(e)?;
                bld.build_int_truncate(hi, t, "")
            }
            op => return Err(format!("LLVM backend: unsupported binary op {op}")),
        }
        .map_err(e)
    }

    fn inst(&mut self, inst: ir::Inst) -> R<()> {
        let f = self.f;
        let dfg = &f.dfg;
        let data = &dfg.insts[inst];
        let op = data.opcode();
        let args = dfg.inst_args(inst).to_vec();
        match data {
            D::UnaryImm { imm, .. } => {
                let r = self.res(inst);
                let t = self.int_ty(r)?;
                self.set(r, t.const_int(imm.bits() as u64, true).into())?;
            }
            D::UnaryIeee32 { imm, .. } => {
                let r = self.res(inst);
                let x = self
                    .cx
                    .c
                    .f32_type()
                    .const_float(f32::from_bits(imm.bits()) as f64);
                self.set(r, x.into())?;
            }
            D::UnaryIeee64 { imm, .. } => {
                let r = self.res(inst);
                let x = self.cx.c.f64_type().const_float(f64::from_bits(imm.bits()));
                self.set(r, x.into())?;
            }
            D::Binary { .. } if op == O::Iadd || op == O::Isub => {
                let r = self.res(inst);
                let (a, b) = (args[0], args[1]);
                let is_p = |x: Value| matches!(self.get(x), Ok(BasicValueEnum::PointerValue(_)));
                if self.ptrs.contains(&r) && (is_p(a) || (op == O::Iadd && is_p(b))) {
                    let (base, off) = if is_p(a) { (a, b) } else { (b, a) };
                    let mut off = self.int(off)?;
                    if op == O::Isub {
                        off = self.b.build_int_neg(off, "").map_err(e)?;
                    }
                    let p = self.gep(self.ptr(base)?, off)?;
                    self.set(r, p.into())?;
                } else {
                    let x = self.binary(op, self.int(a)?, self.int(b)?)?;
                    self.set(r, x.into())?;
                }
            }
            D::Binary { .. } => match op {
                O::Fadd | O::Fsub | O::Fmul | O::Fdiv => {
                    let (a, b) = (self.flt(args[0])?, self.flt(args[1])?);
                    let x = match op {
                        O::Fadd => self.b.build_float_add(a, b, ""),
                        O::Fsub => self.b.build_float_sub(a, b, ""),
                        O::Fmul => self.b.build_float_mul(a, b, ""),
                        _ => self.b.build_float_div(a, b, ""),
                    }
                    .map_err(e)?;
                    self.set(self.res(inst), x.into())?;
                }
                O::Fmin | O::Fmax | O::Fcopysign => {
                    let (a, b) = (self.flt(args[0])?, self.flt(args[1])?);
                    let n = match op {
                        O::Fmin => "llvm.minimum",
                        O::Fmax => "llvm.maximum",
                        _ => "llvm.copysign",
                    };
                    let x = self.call_intr(n, &[a.get_type().into()], &[a.into(), b.into()])?;
                    self.set(self.res(inst), x)?;
                }
                _ => {
                    let a = self.int(args[0])?;
                    let b = self.int(args[1])?;
                    let x = self.binary(op, a, b)?;
                    self.set(self.res(inst), x.into())?;
                }
            },
            D::BinaryImm64 { imm, arg, .. } => {
                let r = self.res(inst);
                if op == O::IaddImm
                    && self.ptrs.contains(&r)
                    && matches!(self.get(*arg), Ok(BasicValueEnum::PointerValue(_)))
                {
                    let p = self.addr(*arg, imm.bits())?;
                    self.set(r, p.into())?;
                } else {
                    let a = self.int(*arg)?;
                    let b = a.get_type().const_int(imm.bits() as u64, true);
                    let x = self.binary(op, a, b)?;
                    self.set(r, x.into())?;
                }
            }
            D::IntCompare { cond, .. } => {
                let (a, b) = (self.int(args[0])?, self.int(args[1])?);
                let c = self
                    .b
                    .build_int_compare(int_pred(*cond), a, b, "")
                    .map_err(e)?;
                self.set(self.res(inst), self.bool8(c)?)?;
            }
            D::IntCompareImm { cond, arg, imm, .. } => {
                let a = self.int(*arg)?;
                let b = a.get_type().const_int(imm.bits() as u64, true);
                let c = self
                    .b
                    .build_int_compare(int_pred(*cond), a, b, "")
                    .map_err(e)?;
                self.set(self.res(inst), self.bool8(c)?)?;
            }
            D::FloatCompare { cond, .. } => {
                let (a, b) = (self.flt(args[0])?, self.flt(args[1])?);
                let c = self
                    .b
                    .build_float_compare(float_pred(*cond), a, b, "")
                    .map_err(e)?;
                self.set(self.res(inst), self.bool8(c)?)?;
            }
            D::Ternary { .. } if op == O::Select => {
                let r = self.res(inst);
                let c = self.nonzero(args[0])?;
                let ty = self.vty(r)?;
                let a = self.coerce(self.get(args[1])?, ty)?;
                let b = self.coerce(self.get(args[2])?, ty)?;
                let x = self.b.build_select(c, a, b, "").map_err(e)?;
                self.set(r, x)?;
            }
            D::Ternary { .. } if op == O::Fma => {
                let (a, b, c) = (self.flt(args[0])?, self.flt(args[1])?, self.flt(args[2])?);
                let x = self.call_intr(
                    "llvm.fma",
                    &[a.get_type().into()],
                    &[a.into(), b.into(), c.into()],
                )?;
                self.set(self.res(inst), x)?;
            }
            D::Unary { arg, .. } => self.unary(inst, op, *arg)?,
            D::LoadNoOffset { arg, .. } if op == O::Bitcast => self.unary(inst, op, *arg)?,
            D::Load {
                arg, flags, offset, ..
            } => {
                let r = self.res(inst);
                let off: i64 = (*offset).into();
                let p = self.addr(*arg, off)?;
                let narrow = match op {
                    O::Uload8 | O::Sload8 => Some(self.cx.c.i8_type()),
                    O::Uload16 | O::Sload16 => Some(self.cx.c.i16_type()),
                    O::Uload32 | O::Sload32 => Some(self.cx.c.i32_type()),
                    O::Load => None,
                    op => return Err(format!("LLVM backend: unsupported load {op}")),
                };
                let entry = f.layout.entry_block();
                let param_idx =
                    |v: Value| entry.and_then(|e| dfg.block_params(e).iter().position(|&x| x == v));
                let refval = match param_idx(dfg.resolve_aliases(*arg)) {
                    Some(k)
                        if off == 0 && narrow.is_none() && f.layout.inst_block(inst) == entry =>
                    {
                        self.refvals.get(&k).copied()
                    }
                    _ => None,
                };
                match narrow {
                    None if refval.is_some() => self.set(r, refval.unwrap())?,
                    None => {
                        let x = self.load(self.vty(r)?, p, flags.aligned())?;
                        self.set(r, x)?;
                    }
                    Some(nt) => {
                        let x = self.load(nt.into(), p, flags.aligned())?.into_int_value();
                        let t = self.int_ty(r)?;
                        let x = if matches!(op, O::Sload8 | O::Sload16 | O::Sload32) {
                            self.b.build_int_s_extend(x, t, "")
                        } else {
                            self.b.build_int_z_extend(x, t, "")
                        }
                        .map_err(e)?;
                        self.set(r, x.into())?;
                    }
                }
            }
            D::Store { flags, offset, .. } => {
                let off: i64 = (*offset).into();
                let p = self.addr(args[1], off)?;
                let x = match op {
                    O::Store => self.get(args[0])?,
                    O::Istore8 => self
                        .cast_int(self.int(args[0])?, self.cx.c.i8_type())?
                        .into(),
                    O::Istore16 => self
                        .cast_int(self.int(args[0])?, self.cx.c.i16_type())?
                        .into(),
                    O::Istore32 => self
                        .cast_int(self.int(args[0])?, self.cx.c.i32_type())?
                        .into(),
                    op => return Err(format!("LLVM backend: unsupported store {op}")),
                };
                self.store(x, p, flags.aligned())?;
            }
            D::StackLoad {
                stack_slot, offset, ..
            } => {
                let r = self.res(inst);
                let off: i64 = (*offset).into();
                let p = self.slot_addr(*stack_slot, off)?;
                if op == O::StackAddr {
                    self.set(r, p.into())?;
                } else {
                    let x = self.load(self.vty(r)?, p, true)?;
                    self.set(r, x)?;
                }
            }
            D::StackStore {
                arg,
                stack_slot,
                offset,
                ..
            } => {
                let off: i64 = (*offset).into();
                let p = self.slot_addr(*stack_slot, off)?;
                self.store(self.get(*arg)?, p, true)?;
            }
            D::UnaryGlobalValue { global_value, .. } => {
                let r = self.res(inst);
                let p = self.global(*global_value)?;
                self.set(r, p.into())?;
            }
            D::FuncAddr { func_ref, .. } => {
                let r = self.res(inst);
                let fv = self
                    .callee(*func_ref)?
                    .ok_or("LLVM backend: address of a library call")?;
                self.set(r, fv.as_global_value().as_pointer_value().into())?;
            }
            D::Call { func_ref, .. } => self.call(inst, *func_ref, &args)?,
            D::CallIndirect { sig_ref, .. } => {
                let sig = &dfg.signatures[*sig_ref];
                let ft = self.cx.fn_type(sig, true)?;
                let callee = self.ptr(args[0])?;
                let a = self.call_args(&args[1..], &ft.get_param_types())?;
                let cs = self.b.build_indirect_call(ft, callee, &a, "").map_err(e)?;
                self.results(inst, cs)?;
            }
            D::MultiAry { .. } if op == O::Return => {
                let rt = self.fv.get_type().get_return_type();
                match args.len() {
                    0 if self.ret_slot.is_some() => {
                        let (a, t) = (self.ret_slot.unwrap(), self.ret_ty.unwrap());
                        let x = self.b.build_load(t, a, "").map_err(e)?;
                        self.b.build_return(Some(&x)).map_err(e)?;
                    }
                    0 => {
                        self.b.build_return(None).map_err(e)?;
                    }
                    1 => {
                        let x = self.coerce(self.get(args[0])?, rt.unwrap())?;
                        self.b.build_return(Some(&x)).map_err(e)?;
                    }
                    _ => {
                        let st = rt.unwrap().into_struct_type();
                        let xs: Vec<BasicValueEnum> = args
                            .iter()
                            .zip(st.get_field_types())
                            .map(|(a, t)| self.coerce(self.get(*a)?, t))
                            .collect::<R<_>>()?;
                        self.b.build_aggregate_return(&xs).map_err(e)?;
                    }
                }
            }
            D::Jump { destination, .. } => {
                let d = self.edge(destination)?;
                self.b.build_unconditional_branch(d).map_err(e)?;
            }
            D::Brif { arg, blocks, .. } => {
                let c = self.nonzero(*arg)?;
                let pool = &dfg.value_lists;
                let same = blocks[0].block(pool) == blocks[1].block(pool);
                let t = self.edge(&blocks[0])?;
                let el = if same {
                    self.split_edge(&blocks[1])?
                } else {
                    self.edge(&blocks[1])?
                };
                let br = self.b.build_conditional_branch(c, t, el).map_err(e)?;
                let cold = |b: &ir::BlockCall| f.layout.is_cold(b.block(pool));
                match (cold(&blocks[0]), cold(&blocks[1])) {
                    (true, false) => self.weights(br, 1, 2000),
                    (false, true) => self.weights(br, 2000, 1),
                    _ => {}
                }
            }
            D::BranchTable { arg, table, .. } => {
                let jt = &dfg.jump_tables[*table];
                let x = self.int(*arg)?;
                let def = self.split_edge(&jt.default_block())?;
                let mut cases = Vec::new();
                for (i, bc) in jt.as_slice().iter().enumerate() {
                    let bb = self.split_edge(bc)?;
                    cases.push((x.get_type().const_int(i as u64, false), bb));
                }
                self.b.build_switch(x, def, &cases).map_err(e)?;
            }
            D::Trap { .. } => {
                let t = self.cx.intrinsic("llvm.trap", &[])?;
                self.b.build_call(t, &[], "").map_err(e)?;
                self.b.build_unreachable().map_err(e)?;
            }
            D::CondTrap { arg, .. } => {
                let mut c = self.nonzero(*arg)?;
                if op == O::Trapz {
                    c = self.b.build_not(c, "").map_err(e)?;
                }
                let trap = self.trap_block()?;
                let cur = self.b.get_insert_block().unwrap();
                let cont = self.cx.c.insert_basic_block_after(cur, "");
                let br = self.b.build_conditional_branch(c, trap, cont).map_err(e)?;
                self.weights(br, 1, 2000);
                self.b.position_at_end(cont);
            }
            D::NullAry { .. } if op == O::Nop => {}
            _ => {
                return Err(format!(
                    "LLVM backend: unsupported instruction `{}`",
                    f.dfg.display_inst(inst)
                ))
            }
        }
        Ok(())
    }

    fn weights(&self, br: InstructionValue<'c>, t: u32, f: u32) {
        let c = self.cx.c;
        let i32t = c.i32_type();
        let md = c.metadata_node(&[
            c.metadata_string("branch_weights").into(),
            i32t.const_int(t as u64, false).into(),
            i32t.const_int(f as u64, false).into(),
        ]);
        let _ = br.set_metadata(md, c.get_kind_id("prof"));
    }

    fn unary(&mut self, inst: ir::Inst, op: O, a: Value) -> R<()> {
        let r = self.res(inst);
        let rty = self.cx.ty(self.f.dfg.value_type(r))?;
        let x: BasicValueEnum = match op {
            O::Ineg => self.b.build_int_neg(self.int(a)?, "").map_err(e)?.into(),
            O::Bnot => self.b.build_not(self.int(a)?, "").map_err(e)?.into(),
            O::Uextend => self
                .b
                .build_int_z_extend(self.int(a)?, rty.into_int_type(), "")
                .map_err(e)?
                .into(),
            O::Sextend => self
                .b
                .build_int_s_extend(self.int(a)?, rty.into_int_type(), "")
                .map_err(e)?
                .into(),
            O::Ireduce => self
                .b
                .build_int_truncate(self.int(a)?, rty.into_int_type(), "")
                .map_err(e)?
                .into(),
            O::Popcnt | O::Bswap | O::Bitrev => {
                let x = self.int(a)?;
                let n = match op {
                    O::Popcnt => "llvm.ctpop",
                    O::Bswap => "llvm.bswap",
                    _ => "llvm.bitreverse",
                };
                self.call_intr(n, &[x.get_type().into()], &[x.into()])?
            }
            O::Clz | O::Ctz => {
                let x = self.int(a)?;
                let n = if op == O::Clz {
                    "llvm.ctlz"
                } else {
                    "llvm.cttz"
                };
                let f = self.cx.c.bool_type().const_zero();
                self.call_intr(n, &[x.get_type().into()], &[x.into(), f.into()])?
            }
            O::Bitcast => {
                let src = self.get(a)?;
                let src = match src {
                    BasicValueEnum::PointerValue(p) => self
                        .b
                        .build_ptr_to_int(p, self.i64t(), "")
                        .map_err(e)?
                        .into(),
                    s => s,
                };
                self.b.build_bit_cast(src, rty, "").map_err(e)?
            }
            O::Fneg => self.b.build_float_neg(self.flt(a)?, "").map_err(e)?.into(),
            O::Fpromote => self
                .b
                .build_float_ext(self.flt(a)?, rty.into_float_type(), "")
                .map_err(e)?
                .into(),
            O::Fdemote => self
                .b
                .build_float_trunc(self.flt(a)?, rty.into_float_type(), "")
                .map_err(e)?
                .into(),
            O::FcvtFromUint => self
                .b
                .build_unsigned_int_to_float(self.int(a)?, rty.into_float_type(), "")
                .map_err(e)?
                .into(),
            O::FcvtFromSint => self
                .b
                .build_signed_int_to_float(self.int(a)?, rty.into_float_type(), "")
                .map_err(e)?
                .into(),
            O::FcvtToSintSat | O::FcvtToUintSat => {
                let x = self.flt(a)?;
                let n = if op == O::FcvtToSintSat {
                    "llvm.fptosi.sat"
                } else {
                    "llvm.fptoui.sat"
                };
                self.call_intr(n, &[rty, x.get_type().into()], &[x.into()])?
            }
            O::Fabs | O::Sqrt | O::Floor | O::Ceil | O::Trunc | O::Nearest => {
                let x = self.flt(a)?;
                let n = match op {
                    O::Fabs => "llvm.fabs",
                    O::Sqrt => "llvm.sqrt",
                    O::Floor => "llvm.floor",
                    O::Ceil => "llvm.ceil",
                    O::Trunc => "llvm.trunc",
                    _ => "llvm.roundeven",
                };
                self.call_intr(n, &[x.get_type().into()], &[x.into()])?
            }
            op => return Err(format!("LLVM backend: unsupported unary op {op}")),
        };
        self.set(r, x)
    }

    fn global(&self, gv: ir::GlobalValue) -> R<PointerValue<'c>> {
        match &self.f.global_values[gv] {
            GlobalValueData::Symbol { name, offset, .. } => {
                let base = match name {
                    ExternalName::User(r) => {
                        let u = &self.f.params.user_named_funcs()[*r];
                        if u.namespace == 0 {
                            self.cx.funcs[&FuncId::from_u32(u.index)]
                                .as_global_value()
                                .as_pointer_value()
                        } else {
                            self.cx.data[&DataId::from_u32(u.index)].as_pointer_value()
                        }
                    }
                    n => return Err(format!("LLVM backend: unsupported symbol {n:?}")),
                };
                let off = offset.bits();
                if off == 0 {
                    Ok(base)
                } else {
                    self.gep(base, self.i64t().const_int(off as u64, true))
                }
            }
            g => Err(format!("LLVM backend: unsupported global value {g}")),
        }
    }

    /// The function a `FuncRef` names, or `None` for a library call.
    fn callee(&self, fr: ir::FuncRef) -> R<Option<FunctionValue<'c>>> {
        match &self.f.dfg.ext_funcs[fr].name {
            ExternalName::User(r) => {
                let u = &self.f.params.user_named_funcs()[*r];
                Ok(Some(self.cx.funcs[&FuncId::from_u32(u.index)]))
            }
            ExternalName::LibCall(_) => Ok(None),
            n => Err(format!("LLVM backend: unsupported callee {n:?}")),
        }
    }

    fn call_args(
        &self,
        args: &[Value],
        tys: &[BasicMetadataTypeEnum<'c>],
    ) -> R<Vec<BasicMetadataValueEnum<'c>>> {
        args.iter()
            .zip(tys)
            .map(|(a, t)| {
                let x = self.get(*a)?;
                let t: BasicTypeEnum = match t {
                    BasicMetadataTypeEnum::PointerType(p) => (*p).into(),
                    BasicMetadataTypeEnum::IntType(i) => (*i).into(),
                    BasicMetadataTypeEnum::FloatType(f) => (*f).into(),
                    _ => x.get_type(),
                };
                Ok(self.coerce(x, t)?.into())
            })
            .collect()
    }

    fn results(&mut self, inst: ir::Inst, cs: inkwell::values::CallSiteValue<'c>) -> R<()> {
        let rs = self.f.dfg.inst_results(inst).to_vec();
        match rs.len() {
            0 => {}
            1 => {
                let x = cs.try_as_basic_value().left().ok_or("call result")?;
                self.set(rs[0], x)?;
            }
            _ => {
                let s = cs
                    .try_as_basic_value()
                    .left()
                    .ok_or("call result")?
                    .into_struct_value();
                for (i, r) in rs.iter().enumerate() {
                    let x = self.b.build_extract_value(s, i as u32, "").map_err(e)?;
                    self.set(*r, x)?;
                }
            }
        }
        Ok(())
    }

    fn call(&mut self, inst: ir::Inst, fr: ir::FuncRef, args: &[Value]) -> R<()> {
        let ext = &self.f.dfg.ext_funcs[fr];
        let fv = match self.callee(fr)? {
            Some(fv) => fv,
            None => {
                let ExternalName::LibCall(lc) = &ext.name else {
                    unreachable!()
                };
                let name = (cranelift_module::default_libcall_names())(*lc);
                match name.as_str() {
                    "memcpy" | "memmove" => {
                        let (d, s, n) =
                            (self.ptr(args[0])?, self.ptr(args[1])?, self.int(args[2])?);
                        let x = if name == "memcpy" {
                            self.b.build_memcpy(d, 1, s, 1, n)
                        } else {
                            self.b.build_memmove(d, 1, s, 1, n)
                        };
                        let x = x.map_err(e)?;
                        let _ = x;
                        return self.libcall_result(inst, d);
                    }
                    "memset" => {
                        let (d, v, n) =
                            (self.ptr(args[0])?, self.int(args[1])?, self.int(args[2])?);
                        let v = self.cast_int(v, self.cx.c.i8_type())?;
                        self.b.build_memset(d, 1, v, n).map_err(e)?;
                        return self.libcall_result(inst, d);
                    }
                    _ => {
                        let sig = &self.f.dfg.signatures[ext.signature];
                        let ft = self.cx.fn_type(sig, false)?;
                        match self.cx.m.get_function(&name) {
                            Some(f) => f,
                            None => self.cx.m.add_function(&name, ft, Some(L::External)),
                        }
                    }
                }
            }
        };
        let a = self.call_args(args, &fv.get_type().get_param_types())?;
        let cs = self.b.build_call(fv, &a, "").map_err(e)?;
        self.results(inst, cs)
    }

    fn libcall_result(&mut self, inst: ir::Inst, d: PointerValue<'c>) -> R<()> {
        if let Some(&r) = self.f.dfg.inst_results(inst).first() {
            self.set(r, d.into())?;
        }
        Ok(())
    }
}

fn natural(t: BasicTypeEnum<'_>) -> u32 {
    match t {
        BasicTypeEnum::IntType(i) => (i.get_bit_width() / 8).clamp(1, 16),
        BasicTypeEnum::FloatType(f) if f == f.get_context().f32_type() => 4,
        _ => 8,
    }
}

fn int_pred(c: IntCC) -> IntPredicate {
    match c {
        IntCC::Equal => IntPredicate::EQ,
        IntCC::NotEqual => IntPredicate::NE,
        IntCC::SignedLessThan => IntPredicate::SLT,
        IntCC::SignedGreaterThanOrEqual => IntPredicate::SGE,
        IntCC::SignedGreaterThan => IntPredicate::SGT,
        IntCC::SignedLessThanOrEqual => IntPredicate::SLE,
        IntCC::UnsignedLessThan => IntPredicate::ULT,
        IntCC::UnsignedGreaterThanOrEqual => IntPredicate::UGE,
        IntCC::UnsignedGreaterThan => IntPredicate::UGT,
        IntCC::UnsignedLessThanOrEqual => IntPredicate::ULE,
    }
}

fn float_pred(c: FloatCC) -> FloatPredicate {
    match c {
        FloatCC::Ordered => FloatPredicate::ORD,
        FloatCC::Unordered => FloatPredicate::UNO,
        FloatCC::Equal => FloatPredicate::OEQ,
        FloatCC::NotEqual => FloatPredicate::UNE,
        FloatCC::OrderedNotEqual => FloatPredicate::ONE,
        FloatCC::UnorderedOrEqual => FloatPredicate::UEQ,
        FloatCC::LessThan => FloatPredicate::OLT,
        FloatCC::LessThanOrEqual => FloatPredicate::OLE,
        FloatCC::GreaterThan => FloatPredicate::OGT,
        FloatCC::GreaterThanOrEqual => FloatPredicate::OGE,
        FloatCC::UnorderedOrLessThan => FloatPredicate::ULT,
        FloatCC::UnorderedOrLessThanOrEqual => FloatPredicate::ULE,
        FloatCC::UnorderedOrGreaterThan => FloatPredicate::UGT,
        FloatCC::UnorderedOrGreaterThanOrEqual => FloatPredicate::UGE,
    }
}

/// Parameter attributes from [`crate::ParamHint`]s.
fn param_attrs<'c>(cx: &Cx<'c>, fv: FunctionValue<'c>, hints: &[crate::ParamHint]) {
    let n = fv.count_params();
    for h in hints {
        if h.index as u32 >= n
            || !fv
                .get_nth_param(h.index as u32)
                .is_some_and(|p| p.is_pointer_value())
        {
            continue;
        }
        let loc = AttributeLoc::Param(h.index as u32);
        if h.noalias {
            fv.add_attribute(loc, cx.enum_attr("noalias"));
        }
        if h.size > 0 {
            let k = Attribute::get_named_enum_kind_id("dereferenceable");
            fv.add_attribute(loc, cx.c.create_enum_attribute(k, h.size as u64));
        }
        if h.align > 1 {
            let k = Attribute::get_named_enum_kind_id("align");
            fv.add_attribute(loc, cx.c.create_enum_attribute(k, h.align as u64));
        }
    }
}

/// The reference hints that can be applied: the slot parameter must only
/// be read (every use is a load's address), and the reference is read at
/// offset 0 in the entry block.
fn valid_refs(f: &ir::Function, hints: &crate::FnHints) -> Vec<crate::RefHint> {
    let Some(entry) = f.layout.entry_block() else {
        return vec![];
    };
    let params = f.dfg.block_params(entry);
    let mut ok = Vec::new();
    for r in &hints.refs {
        let Some(&pv) = params.get(r.param) else {
            continue;
        };
        if f.dfg.value_type(pv) != types::I64 {
            continue;
        }
        let mut only_loads = true;
        let mut read_at_entry = false;
        for b in f.layout.blocks() {
            for inst in f.layout.block_insts(b) {
                let args = f.dfg.inst_args(inst);
                if !args.contains(&pv) {
                    // Branch arguments are not in `inst_args`.
                    if ptrs::branch_args(f, inst)
                        .iter()
                        .any(|(_, a)| a.contains(&pv))
                    {
                        only_loads = false;
                    }
                    continue;
                }
                match &f.dfg.insts[inst] {
                    D::Load {
                        arg,
                        offset,
                        opcode: O::Load,
                        ..
                    } if *arg == pv => {
                        let off: i64 = (*offset).into();
                        if off == 0
                            && b == entry
                            && f.dfg.value_type(f.dfg.first_result(inst)) == types::I64
                        {
                            read_at_entry = true;
                        }
                    }
                    D::Load { arg, .. } if *arg == pv => {}
                    _ => only_loads = false,
                }
            }
        }
        if only_loads && read_at_entry {
            ok.push(*r);
        }
    }
    ok
}

/// Makes `fv` a thunk that loads the reference arguments and calls a new
/// body function taking them as `noalias` parameters; returns the body.
fn thunk<'c>(
    cx: &Cx<'c>,
    fv: FunctionValue<'c>,
    name: &str,
    f: &Func,
    refs: &[crate::RefHint],
) -> R<FunctionValue<'c>> {
    let ft = fv.get_type();
    let mut ps = ft.get_param_types();
    let n = ps.len();
    for _ in refs {
        ps.push(cx.ptr_t().into());
    }
    let bt = match ft.get_return_type() {
        Some(t) => t.fn_type(&ps, false),
        None => cx.c.void_type().fn_type(&ps, false),
    };
    let body =
        cx.m.add_function(&format!("{name}.body"), bt, Some(L::Internal));
    param_attrs(cx, body, &f.hints.params);
    for (j, r) in refs.iter().enumerate() {
        let loc = AttributeLoc::Param((n + j) as u32);
        body.add_attribute(loc, cx.enum_attr("noalias"));
        body.add_attribute(loc, cx.enum_attr("nonnull"));
        body.add_attribute(loc, cx.enum_attr("noundef"));
        if !r.mutable {
            body.add_attribute(loc, cx.enum_attr("readonly"));
        }
        if r.size > 0 {
            let k = Attribute::get_named_enum_kind_id("dereferenceable");
            body.add_attribute(loc, cx.c.create_enum_attribute(k, r.size as u64));
        }
        if r.align > 1 {
            let k = Attribute::get_named_enum_kind_id("align");
            body.add_attribute(loc, cx.c.create_enum_attribute(k, r.align as u64));
        }
    }
    let b = cx.c.create_builder();
    let bb = cx.c.append_basic_block(fv, "entry");
    b.position_at_end(bb);
    let mut args: Vec<BasicMetadataValueEnum> = fv.get_param_iter().map(|p| p.into()).collect();
    for r in refs {
        let slot = fv
            .get_nth_param(r.param as u32)
            .ok_or("thunk parameter")?
            .into_pointer_value();
        let x = b.build_load(cx.ptr_t(), slot, "").map_err(e)?;
        let _ = x.as_instruction_value().unwrap().set_alignment(8);
        args.push(x.into());
    }
    let cs = b.build_call(body, &args, "").map_err(e)?;
    match cs.try_as_basic_value().left() {
        Some(v) => b.build_return(Some(&v)).map_err(e)?,
        None => b.build_return(None).map_err(e)?,
    };
    fv.add_attribute(AttributeLoc::Function, cx.enum_attr("alwaysinline"));
    fv.add_attribute(AttributeLoc::Function, cx.enum_attr("nounwind"));
    param_attrs(cx, fv, &f.hints.params);
    Ok(body)
}

/// Functions whose address escapes (function pointers, vtables, data):
/// their pointer-slot signature must stay as it is.
fn address_taken(funcs: &[Func], data: &[(DataId, DataDescription)]) -> HashSet<FuncId> {
    let mut out = HashSet::new();
    let id_of = |f: &ir::Function, fr: ir::FuncRef| match &f.dfg.ext_funcs[fr].name {
        ExternalName::User(r) => Some(FuncId::from_u32(f.params.user_named_funcs()[*r].index)),
        _ => None,
    };
    for func in funcs {
        let f = &func.func;
        for blk in f.layout.blocks() {
            for inst in f.layout.block_insts(blk) {
                if let D::FuncAddr { func_ref, .. } = f.dfg.insts[inst] {
                    out.extend(id_of(f, func_ref));
                }
            }
        }
    }
    for (_, d) in data {
        for (_, fr) in &d.function_relocs {
            if let Ok((_, idx)) = reloc_target(&d.function_decls[*fr]) {
                out.insert(FuncId::from_u32(idx));
            }
        }
        for (_, gv, _) in &d.data_relocs {
            if let Ok((0, idx)) = reloc_target(&d.data_decls[*gv]) {
                out.insert(FuncId::from_u32(idx));
            }
        }
    }
    out
}

/// The register type a function's result can be returned in instead of
/// through its return slot (parameter 0): only for internal functions that
/// are called directly, with a 1/2/4/8/16-byte result.
fn promoted_ret<'c>(cx: &Cx<'c>, f: &Func, eligible: bool) -> Option<BasicTypeEnum<'c>> {
    if !eligible
        || !f.func.signature.returns.is_empty()
        || std::env::var_os("KILN_LLVM_NO_RETS").is_some()
    {
        return None;
    }
    let h = f.hints.params.iter().find(|h| h.index == 0 && !h.noalias)?;
    matches!(h.size, 1 | 2 | 4 | 8 | 16).then(|| cx.c.custom_width_int_type(h.size * 8).into())
}

/// Turns `fv` (the pointer-slot entry point every caller uses) into an
/// always-inlined wrapper that calls `{name}.v`, which returns the result by
/// value, and stores it into the slot. After inlining, callers' slots
/// become registers. Returns `{name}.v`, which gets the body.
fn ret_wrapper<'c>(
    cx: &Cx<'c>,
    fv: FunctionValue<'c>,
    name: &str,
    f: &Func,
    rt: BasicTypeEnum<'c>,
) -> R<FunctionValue<'c>> {
    let ps = fv.get_type().get_param_types();
    let inner = cx.m.add_function(
        &format!("{name}.v"),
        rt.fn_type(&ps, false),
        Some(L::Internal),
    );
    param_attrs(cx, inner, &f.hints.params);
    let b = cx.c.create_builder();
    let bb = cx.c.append_basic_block(fv, "entry");
    b.position_at_end(bb);
    let args: Vec<BasicMetadataValueEnum> = fv.get_param_iter().map(|p| p.into()).collect();
    let cs = b.build_call(inner, &args, "").map_err(e)?;
    let v = cs.try_as_basic_value().left().ok_or("return value")?;
    let slot = fv
        .get_nth_param(0)
        .ok_or("return slot")?
        .into_pointer_value();
    let st = b.build_store(slot, v).map_err(e)?;
    let _ = st.set_alignment(1);
    b.build_return(None).map_err(e)?;
    fv.add_attribute(AttributeLoc::Function, cx.enum_attr("alwaysinline"));
    fv.add_attribute(AttributeLoc::Function, cx.enum_attr("nounwind"));
    param_attrs(cx, fv, &f.hints.params);
    Ok(inner)
}
