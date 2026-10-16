//! Which Cranelift values are addresses.
//!
//! Cranelift IR types addresses as plain `i64`. LLVM optimizes much better
//! when they are `ptr` (SROA, alias analysis, and GEP folding all need
//! provenance), so this pass guesses, conservatively in effect: any `i64`
//! can be represented either way (`inttoptr` / `ptrtoint` convert at the
//! edges), so a wrong guess costs optimization, never correctness.
//!
//! A value is a pointer if it is made by `stack_addr`, `global_value`,
//! `symbol_value`, or `func_addr`, is an `i64` parameter of a Kiln function,
//! is used as the address of a load or store or as an indirect callee, or is
//! connected to such a value through address arithmetic (`iadd`, `isub` of
//! an offset), `select`, or a block parameter.

use cranelift_codegen::ir::{self, types, InstructionData, Opcode, Value};
use std::collections::HashSet;

/// Infers the set of pointer values in `f`. `params_are_ptrs` is true for
/// functions defined by Kiln (all of whose `i64` parameters are addresses).
pub fn infer(f: &ir::Function, params_are_ptrs: bool) -> HashSet<Value> {
    let dfg = &f.dfg;
    let mut p: HashSet<Value> = HashSet::new();
    let is64 = |v: Value| dfg.value_type(v) == types::I64;
    if params_are_ptrs {
        if let Some(e) = f.layout.entry_block() {
            for &v in dfg.block_params(e) {
                if is64(v) {
                    p.insert(v);
                }
            }
        }
    }
    // An operand that is clearly an offset or a constant, not a base.
    let offset_like = |v: Value| match dfg.value_def(v) {
        ir::ValueDef::Result(i, _) => matches!(
            dfg.insts[i].opcode(),
            Opcode::Iconst
                | Opcode::Imul
                | Opcode::ImulImm
                | Opcode::Ishl
                | Opcode::IshlImm
                | Opcode::Uextend
                | Opcode::Sextend
                | Opcode::Band
                | Opcode::BandImm
        ),
        _ => false,
    };
    loop {
        let before = p.len();
        for b in f.layout.blocks() {
            for inst in f.layout.block_insts(b) {
                let data = &dfg.insts[inst];
                let op = data.opcode();
                let res = dfg.inst_results(inst);
                let r = |v: Value| dfg.resolve_aliases(v);
                match data {
                    InstructionData::Load { arg, .. } => {
                        p.insert(r(*arg));
                    }
                    InstructionData::LoadNoOffset { arg, opcode, .. }
                        if *opcode != Opcode::Bitcast =>
                    {
                        p.insert(r(*arg));
                    }
                    InstructionData::Store { args, .. }
                    | InstructionData::StoreNoOffset { args, .. } => {
                        if is64(args[1]) {
                            p.insert(r(args[1]));
                        }
                    }
                    InstructionData::CallIndirect { args, .. } => {
                        if let Some(&c) = args.as_slice(&dfg.value_lists).first() {
                            p.insert(r(c));
                        }
                    }
                    _ => {}
                }
                match op {
                    Opcode::StackAddr
                    | Opcode::GlobalValue
                    | Opcode::SymbolValue
                    | Opcode::FuncAddr => {
                        if is64(res[0]) {
                            p.insert(res[0]);
                        }
                    }
                    Opcode::Iadd => {
                        let a: Vec<Value> = dfg.inst_args(inst).iter().map(|&v| r(v)).collect();
                        let rr = res[0];
                        if p.contains(&a[0]) || p.contains(&a[1]) {
                            p.insert(rr);
                        } else if p.contains(&rr) {
                            // Demanded: the base is the operand that isn't an offset.
                            if offset_like(a[0]) && !offset_like(a[1]) {
                                p.insert(a[1]);
                            } else {
                                p.insert(a[0]);
                            }
                        }
                    }
                    Opcode::IaddImm => {
                        let a = r(dfg.inst_args(inst)[0]);
                        if is64(a) {
                            if p.contains(&a) {
                                p.insert(res[0]);
                            } else if p.contains(&res[0]) {
                                p.insert(a);
                            }
                        }
                    }
                    Opcode::Isub => {
                        let a: Vec<Value> = dfg.inst_args(inst).iter().map(|&v| r(v)).collect();
                        // `p - offset` stays a pointer; `p - q` is a distance.
                        if p.contains(&a[0]) && !p.contains(&a[1]) {
                            p.insert(res[0]);
                        } else if p.contains(&res[0]) {
                            p.insert(a[0]);
                        }
                    }
                    Opcode::Select => {
                        let a: Vec<Value> = dfg.inst_args(inst).iter().map(|&v| r(v)).collect();
                        if is64(res[0])
                            && (p.contains(&a[1]) || p.contains(&a[2]) || p.contains(&res[0]))
                        {
                            p.insert(res[0]);
                            p.insert(a[1]);
                            p.insert(a[2]);
                        }
                    }
                    _ => {}
                }
                // Block arguments and parameters agree.
                for (dest, args) in branch_args(f, inst) {
                    let params = dfg.block_params(dest);
                    for (&a, &bp) in args.iter().zip(params) {
                        let a = r(a);
                        if !is64(bp) {
                            continue;
                        }
                        if p.contains(&a) || p.contains(&bp) {
                            p.insert(a);
                            p.insert(bp);
                        }
                    }
                }
            }
        }
        if p.len() == before {
            return p;
        }
    }
}

/// The destinations of a branch instruction with their arguments.
pub fn branch_args(f: &ir::Function, inst: ir::Inst) -> Vec<(ir::Block, Vec<Value>)> {
    let dfg = &f.dfg;
    let pool = &dfg.value_lists;
    let call = |bc: &ir::BlockCall| (bc.block(pool), bc.args_slice(pool).to_vec());
    match &dfg.insts[inst] {
        InstructionData::Jump { destination, .. } => vec![call(destination)],
        InstructionData::Brif { blocks, .. } => blocks.iter().map(call).collect(),
        InstructionData::BranchTable { table, .. } => dfg.jump_tables[*table]
            .all_branches()
            .iter()
            .map(call)
            .collect(),
        _ => vec![],
    }
}
