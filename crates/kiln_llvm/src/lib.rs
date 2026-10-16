//! The optional LLVM backend for Kiln release builds.
//!
//! `kiln_codegen` lowers MIR to Cranelift IR either way. With this backend,
//! instead of compiling that IR with Cranelift, every function and data
//! object is translated into one LLVM module, optimized with LLVM's `O3`
//! pipeline, and emitted as an object file. Reusing the Cranelift IR means
//! the two backends share one lowering of the language, so every feature
//! works the same under both; the translation only has to map a small set
//! of Cranelift instructions.
//!
//! Cranelift IR has no pointer type (addresses are `i64`), but LLVM's alias
//! analysis and SROA need real pointers, so [`ptrs`] infers which values are
//! addresses and the translation keeps them as `ptr`, using `getelementptr`
//! for address arithmetic. Kiln's own guarantees are passed on as LLVM
//! attributes and metadata (see [`FnHints`]): argument slots are `noalias`,
//! and `&mut T` / `&T` arguments get alias scopes, the way `rustc` marks
//! them `noalias`.

#[cfg(feature = "llvm")]
mod ptrs;
#[cfg(feature = "llvm")]
mod translate;

use cranelift_codegen::ir;
use cranelift_module::{DataDescription, DataId, FuncId, ModuleDeclarations};

/// What Kiln knows about a function's parameters that Cranelift IR cannot say.
#[derive(Debug, Clone, Default)]
pub struct FnHints {
    /// Facts about pointer parameters (the return slot, argument slots).
    pub params: Vec<ParamHint>,
    /// Reference arguments: the parameter's slot holds a `&T` / `&mut T`
    /// (or the data pointer of a `&[T]` / `&str`) at offset 0.
    pub refs: Vec<RefHint>,
    /// A small function worth inlining.
    pub inline: bool,
}

/// A pointer parameter.
#[derive(Debug, Clone, Copy)]
pub struct ParamHint {
    /// Parameter index.
    pub index: usize,
    /// The slot is fresh for this call (an argument slot), so nothing else
    /// reaches that memory while the function runs.
    pub noalias: bool,
    /// Bytes known to be valid at the pointer (0 if unknown).
    pub size: u32,
    /// Known alignment.
    pub align: u32,
}

/// A reference argument.
///
/// The LLVM function for such a Kiln function is a small always-inlined
/// thunk that loads the reference out of its slot and passes it to the
/// real body as a `noalias` parameter, so LLVM's own (capture-aware) alias
/// analysis and inliner apply Kiln's borrow rules: within the call, memory
/// behind a `&mut T` is reached only through it, and memory behind a `&T`
/// (of a type without interior mutability) is not written at all.
#[derive(Debug, Clone, Copy)]
pub struct RefHint {
    /// Parameter index of the slot holding the reference.
    pub param: usize,
    /// `&mut T`.
    pub mutable: bool,
    /// Size of `T` (0 for slices and `str`).
    pub size: u32,
    /// Alignment of `T`.
    pub align: u32,
}

/// One function to translate.
pub struct Func {
    /// The Cranelift function id (its declaration is in the module).
    pub id: FuncId,
    /// The function body, as built by `kiln_codegen`.
    pub func: ir::Function,
    /// Facts from the Kiln types.
    pub hints: FnHints,
}

/// Build settings for the LLVM backend.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Target CPU (`"native"` for the host); defaults to the baseline
    /// `x86-64` / `generic`, as `rustc` does.
    pub cpu: Option<String>,
    /// Write the optimized LLVM IR here, for inspection.
    pub dump_ir: Option<std::path::PathBuf>,
}

/// Translates and optimizes the whole program, returning object file bytes.
pub fn emit(
    decls: &ModuleDeclarations,
    funcs: &[Func],
    data: &[(DataId, DataDescription)],
    settings: &Settings,
) -> Result<Vec<u8>, String> {
    #[cfg(feature = "llvm")]
    return translate::emit(decls, funcs, data, settings);
    #[cfg(not(feature = "llvm"))]
    {
        let _ = (decls, funcs, data, settings);
        Err("kiln_llvm was built without its `llvm` feature".into())
    }
}
