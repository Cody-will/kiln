//! Promoting stack slots to SSA values (release builds).
//!
//! The translator gives every MIR local (and many temporaries) a stack slot
//! and reads and writes it with loads and stores. Cranelift has no pass that
//! turns such slots into registers, so tight loops shuffle every value
//! through memory. This pass fixes that in two steps:
//!
//! 1. The function is translated once, normally. Every load and store the
//!    translator emits through [`FnCx::load`]/[`FnCx::store`] is recorded.
//!    [`Promo::analyze`] then finds the slots whose address never escapes:
//!    it is used only as the address of those recorded accesses (possibly
//!    through `iadd_imm`), every access is 1, 2, 4, or 8 bytes, and any two
//!    accesses either cover exactly the same bytes or don't overlap.
//! 2. The function is translated again. Accesses to a promoted slot become
//!    reads and writes of a Cranelift `Variable` per (slot, offset), and the
//!    frontend's SSA construction does the rest. Each variable has the
//!    integer type of its size; float accesses are bitcast.
//!
//! The translation is deterministic, so slot `k` is the same slot in both
//! passes.

use crate::func::FnCx;
use cranelift_codegen::ir::{self, types, InstBuilder, InstructionData, MemFlags, Opcode, Value};
use cranelift_frontend::Variable;
use cranelift_module::Module;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct Promo {
    /// Whether promotion is on at all (release builds).
    pub enabled: bool,
    /// Pass 2: the slots to promote.
    pub promoted: Option<HashSet<u32>>,
    /// `stack_addr` results of the slots made by [`FnCx::slot`], by slot number.
    pub bases: HashMap<Value, u32>,
    pub next_slot: u32,
    /// Pass 1: the loads and stores emitted through the helpers.
    pub accesses: HashSet<ir::Inst>,
    pub vars: HashMap<(u32, i64, u32), Variable>,
    pub next_var: u32,
}

fn int_of(bytes: u32) -> Option<ir::Type> {
    match bytes {
        1 => Some(types::I8),
        2 => Some(types::I16),
        4 => Some(types::I32),
        8 => Some(types::I64),
        _ => None,
    }
}

impl Promo {
    /// The slot and byte offset an address points into, if it is (an
    /// `iadd_imm` of) a slot's base address.
    pub fn resolve(&self, func: &ir::Function, mut v: Value) -> Option<(u32, i64)> {
        let mut off = 0i64;
        for _ in 0..16 {
            if let Some(s) = self.bases.get(&v) {
                return Some((*s, off));
            }
            let inst = func.dfg.value_def(v).inst()?;
            match func.dfg.insts[inst] {
                InstructionData::BinaryImm64 { opcode: Opcode::IaddImm, arg, imm } => {
                    off += imm.bits();
                    v = arg;
                }
                _ => return None,
            }
        }
        None
    }

    /// Pass 1: the slots that can live in SSA values.
    pub fn analyze(&self, func: &ir::Function) -> HashSet<u32> {
        let mut bad: HashSet<u32> = HashSet::new();
        let mut acc: HashMap<u32, Vec<(i64, u32)>> = HashMap::new();
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                let data = &func.dfg.insts[inst];
                let args: Vec<Value> = func.dfg.inst_values(inst).collect();
                let recorded = self.accesses.contains(&inst);
                match (data.opcode(), recorded) {
                    (Opcode::IaddImm, _) => {} // its uses are checked in turn
                    (Opcode::Load, true) => {
                        if let InstructionData::Load { arg, offset, .. } = *data {
                            if let Some((s, o)) = self.resolve(func, arg) {
                                let ty = func.dfg.value_type(func.dfg.first_result(inst));
                                acc.entry(s).or_default().push((o + i64::from(i32::from(offset)), ty.bytes()));
                            }
                        }
                    }
                    (Opcode::Store, true) => {
                        if let InstructionData::Store { args: [val, addr], offset, .. } = *data {
                            if let Some((s, _)) = self.resolve(func, val) {
                                bad.insert(s); // the address itself is stored
                            }
                            if let Some((s, o)) = self.resolve(func, addr) {
                                let ty = func.dfg.value_type(val);
                                acc.entry(s).or_default().push((o + i64::from(i32::from(offset)), ty.bytes()));
                            }
                        }
                    }
                    _ => {
                        for a in args {
                            if let Some((s, _)) = self.resolve(func, a) {
                                bad.insert(s);
                            }
                        }
                    }
                }
            }
        }
        let mut out = HashSet::new();
        for (s, mut v) in acc {
            if bad.contains(&s) {
                continue;
            }
            v.sort();
            v.dedup();
            let sizes_ok = v.iter().all(|(o, n)| int_of(*n).is_some() && *o >= 0);
            let disjoint = v.windows(2).all(|w| w[0].0 + i64::from(w[0].1) <= w[1].0);
            if sizes_ok && disjoint {
                out.insert(s);
            }
        }
        out
    }
}

impl FnCx<'_, '_, '_, '_> {
    /// The promoted variable for an access, if the address is in a promoted slot.
    fn promoted_var(&mut self, addr: Value, off: i32, bytes: u32) -> Option<(Variable, ir::Type)> {
        let set = self.promo.promoted.as_ref()?;
        let (s, o) = self.promo.resolve(self.b.func, addr)?;
        if !set.contains(&s) {
            return None;
        }
        let ity = int_of(bytes)?;
        let key = (s, o + i64::from(off), bytes);
        if let Some(v) = self.promo.vars.get(&key) {
            return Some((*v, ity));
        }
        let var = Variable::from_u32(self.promo.next_var);
        self.promo.next_var += 1;
        self.b.declare_var(var, ity);
        self.promo.vars.insert(key, var);
        Some((var, ity))
    }

    pub(crate) fn load(&mut self, t: ir::Type, addr: Value, off: i32) -> Value {
        if self.promo.enabled {
            if let Some((var, ity)) = self.promoted_var(addr, off, t.bytes()) {
                let v = self.b.use_var(var);
                return if t == ity { v } else { self.b.ins().bitcast(t, MemFlags::new(), v) };
            }
        }
        let v = self.b.ins().load(t, crate::func::flags(), addr, off);
        if self.promo.enabled && self.promo.promoted.is_none() {
            let inst = self.b.func.dfg.value_def(v).unwrap_inst();
            self.promo.accesses.insert(inst);
        }
        v
    }

    pub(crate) fn store(&mut self, v: Value, addr: Value, off: i32) {
        if self.promo.enabled {
            let t = self.b.func.dfg.value_type(v);
            if let Some((var, ity)) = self.promoted_var(addr, off, t.bytes()) {
                let v = if t == ity { v } else { self.b.ins().bitcast(ity, MemFlags::new(), v) };
                self.b.def_var(var, v);
                return;
            }
        }
        let inst = self.b.ins().store(crate::func::flags(), v, addr, off);
        if self.promo.enabled && self.promo.promoted.is_none() {
            self.promo.accesses.insert(inst);
        }
    }

    /// Copies `size` bytes. Copies touching a slot go through [`Self::load`]
    /// and [`Self::store`] in aligned chunks, so the slot can stay promotable.
    pub(crate) fn copy(&mut self, dst: Value, src: Value, size: u32) {
        if size == 0 {
            return;
        }
        let touches_slot = self.promo.enabled
            && size <= 64
            && (self.promo.resolve(self.b.func, dst).is_some() || self.promo.resolve(self.b.func, src).is_some());
        if touches_slot {
            let mut off = 0u32;
            while off < size {
                let rest = size - off;
                let n = [8u32, 4, 2, 1].into_iter().find(|n| *n <= rest && off.is_multiple_of(*n)).unwrap_or(1);
                let t = int_of(n).expect("chunk size");
                let v = self.load(t, src, off as i32);
                self.store(v, dst, off as i32);
                off += n;
            }
            return;
        }
        let cfg = self.cg.module.target_config();
        self.b
            .emit_small_memory_copy(cfg, dst, src, size as u64, 1, 1, true, crate::func::flags());
    }
}
