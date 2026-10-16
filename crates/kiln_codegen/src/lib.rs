//! Native code generation for Kiln.
//!
//! The backend is [Cranelift](https://cranelift.dev): it is written in Rust, so
//! the compiler builds with nothing but `cargo` on Linux, macOS, and Windows
//! (no system LLVM install). See `docs/ARCHITECTURE.md` for why it was chosen
//! over LLVM.
//!
//! Pipeline: the front end (parse → resolve → typeck → borrowck) must succeed;
//! then, starting from `main`, each reachable function is **monomorphized**
//! (one copy per set of concrete generic arguments), its MIR is translated to
//! Cranelift IR, and the object file is linked with the runtime library
//! (`kiln_runtime`) by the system linker.
//!
//! Code generation strategy: every MIR local lives in a stack slot and every
//! value is moved through memory, which keeps the translation simple and
//! correct; Cranelift's optimizer cleans up scalar traffic. All Kiln functions
//! use one calling convention: `fn(ret: *mut R, arg0: *mut A0, ...)`, where
//! each argument points at a caller-owned copy that the callee takes over.
//! Closures take an extra environment pointer after `ret`. Drops are driven by
//! per-local drop flags set on initialization and cleared on moves.

mod bce;
mod func;
mod glue;
mod inline;
mod intrinsics;
mod layout;
pub mod link;
mod promote;

use cranelift_codegen::ir::{types, AbiParam, Signature, Type};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::{default_libcall_names, DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use kiln_lexer::{Diagnostic, Span};
use kiln_mir::Body;
use kiln_resolve::Resolved;
use kiln_typeck::ty::Ty;
use kiln_typeck::Typed;
use std::collections::{HashMap, VecDeque};

pub use layout::Layout;

/// How a generated function is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abi {
    /// `fn(ret, args...)`
    Plain,
    /// `fn(ret, env, args...)` (closures and fn-value shims)
    Closure,
}

/// Pending work: functions declared but not yet defined.
pub(crate) enum Work {
    Fn {
        id: FuncId,
        fn_idx: usize,
        body: usize,
        substs: HashMap<String, Ty>,
        abi: Abi,
    },
    /// Structural ordering for a type: `(out: *mut i8 (-1/0/1), a, b)`.
    CmpGlue {
        id: FuncId,
        ty: Ty,
    },
    /// Structural Debug output for a type: `(f: *mut Formatter, value)`.
    DebugGlue {
        id: FuncId,
        ty: Ty,
    },
    /// Structural hashing for a type: `(hasher: *mut u64, value)`.
    HashGlue {
        id: FuncId,
        ty: Ty,
    },
    /// Structural equality for a type: `(out: *mut bool, a, b)`.
    EqGlue {
        id: FuncId,
        ty: Ty,
    },
    /// Structural clone for an ADT: `(dst, src)`.
    CloneGlue {
        id: FuncId,
        ty: Ty,
    },
    DropGlue {
        id: FuncId,
        ty: Ty,
    },
    /// `fn(ret, env, args...)` forwarding to a plain function.
    Shim {
        id: FuncId,
        target: FuncId,
        nargs: usize,
    },
}

/// The code generator for one program.
pub struct Codegen<'r, 'a> {
    /// Panic on integer overflow (debug builds).
    pub(crate) checked: bool,
    pub(crate) typed: Typed<'r, 'a>,
    pub(crate) r: &'r Resolved<'a>,
    pub(crate) module: ObjectModule,
    /// LLVM backend: definitions are recorded here, not compiled.
    pub(crate) capture: Option<Captured>,
    pub(crate) freeze_cache: HashMap<String, bool>,
    pub(crate) bodies: HashMap<usize, Vec<Body>>,
    pub(crate) instances: HashMap<String, FuncId>,
    pub(crate) queue: VecDeque<Work>,
    pub(crate) layouts: HashMap<String, Layout>,
    pub(crate) needs_drop_cache: HashMap<String, bool>,
    pub(crate) drop_glue: HashMap<String, FuncId>,
    pub(crate) eq_glue: HashMap<String, FuncId>,
    pub(crate) cmp_glue: HashMap<String, FuncId>,
    pub(crate) hash_glue: HashMap<String, FuncId>,
    pub(crate) debug_glue: HashMap<String, FuncId>,
    pub(crate) clone_glue: HashMap<String, FuncId>,
    pub(crate) vtables: HashMap<String, DataId>,
    /// `Display::to_string` for `main`'s error type, if it has one.
    pub(crate) entry_display: Option<FuncId>,
    pub(crate) shims: HashMap<FuncId, FuncId>,
    pub(crate) strings: HashMap<String, DataId>,
    /// Static items used by the program: definition → (storage, initializer
    /// function index). Initialized before `main`, in declaration order.
    pub(crate) statics: std::collections::BTreeMap<kiln_resolve::DefId, (DataId, usize)>,
    pub(crate) rt: HashMap<&'static str, FuncId>,
    /// Runtime functions declared by `std` (`kn_*` without a body).
    pub(crate) externs: HashMap<String, FuncId>,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) counter: usize,
    /// Span of the item being compiled, for errors without a better location.
    pub(crate) span: Span,
}

/// Runtime functions: name, parameter types, result type.
const RUNTIME: &[(&str, &[Type], Option<Type>)] = &[
    ("kn_alloc", &[types::I64, types::I64], Some(types::I64)),
    ("kn_free", &[types::I64, types::I64, types::I64], None),
    ("kn_panic", &[types::I64, types::I64], None),
    ("kn_panic_bounds", &[types::I64, types::I64], None),
    ("kn_panic_str", &[types::I64], None),
    (
        "kn_print",
        &[types::I64, types::I64, types::I32, types::I32],
        None,
    ),
    ("kn_main_error", &[types::I64, types::I64], None),
    (
        "kn_vec_reserve",
        &[types::I64, types::I64, types::I64, types::I64],
        None,
    ),
    ("kn_vec_free", &[types::I64, types::I64, types::I64], None),
    (
        "kn_string_push",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    (
        "kn_string_from",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    ("kn_fmt_i64", &[types::I64, types::I64], None),
    ("kn_fmt_u64", &[types::I64, types::I64], None),
    ("kn_fmt_bool", &[types::I64, types::I32], None),
    ("kn_fmt_char", &[types::I64, types::I32], None),
    ("kn_fmt_f64", &[types::I64, types::F64], None),
    (
        "kn_str_eq",
        &[types::I64, types::I64, types::I64, types::I64],
        Some(types::I32),
    ),
    ("kn_hash_u64", &[types::I64, types::I64], None),
    (
        "kn_fmt_debug_str",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    ("kn_fmt_debug_char", &[types::I64, types::I32], None),
    ("kn_fmt_debug_f64", &[types::I64, types::F64], None),
    (
        "kn_fmt_radix",
        &[types::I64, types::I64, types::I32, types::I32, types::I32],
        None,
    ),
    (
        "kn_fmt_f64_fixed",
        &[types::I64, types::F64, types::I64],
        None,
    ),
    (
        "kn_fmt_f64_exp",
        &[types::I64, types::F64, types::I64, types::I32],
        None,
    ),
    ("kn_fmt_begin", &[types::I64, types::I64, types::I64], None),
    ("kn_fmt_sep", &[types::I64, types::I32, types::I32], None),
    (
        "kn_fmt_end",
        &[types::I64, types::I64, types::I64, types::I32, types::I32],
        None,
    ),
    (
        "kn_fmt_pad",
        &[
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::I32,
            types::I32,
            types::I32,
            types::I64,
        ],
        None,
    ),
    ("kn_hash_bytes", &[types::I64, types::I64, types::I64], None),
    (
        "kn_str_cmp",
        &[types::I64, types::I64, types::I64, types::I64],
        Some(types::I32),
    ),
    (
        "kn_str_to_upper",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    (
        "kn_str_to_lower",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    ("kn_str_trim", &[types::I64, types::I64, types::I64], None),
    (
        "kn_str_starts_with",
        &[types::I64, types::I64, types::I64, types::I64],
        Some(types::I32),
    ),
    (
        "kn_str_ends_with",
        &[types::I64, types::I64, types::I64, types::I64],
        Some(types::I32),
    ),
    (
        "kn_str_contains",
        &[types::I64, types::I64, types::I64, types::I64],
        Some(types::I32),
    ),
    ("kn_defer_push", &[types::I64, types::I32, types::I64], None),
    ("kn_defer_pop", &[types::I64], Some(types::I64)),
    // Tasks, channels, select, scopes (kiln_sched.c).
    (
        "kn_go",
        &[types::I64, types::I64, types::I64, types::I64, types::I64],
        Some(types::I64),
    ),
    ("kn_task_join", &[types::I64, types::I64], None),
    ("kn_task_drop", &[types::I64], None),
    ("kn_scope_new", &[], Some(types::I64)),
    (
        "kn_scope_spawn",
        &[types::I64, types::I64, types::I64],
        None,
    ),
    ("kn_scope_wait", &[types::I64], None),
    (
        "kn_chan_new",
        &[types::I64, types::I64, types::I64],
        Some(types::I64),
    ),
    ("kn_chan_send", &[types::I64, types::I64], None),
    ("kn_chan_recv", &[types::I64, types::I64], Some(types::I32)),
    (
        "kn_chan_try_send",
        &[types::I64, types::I64],
        Some(types::I32),
    ),
    (
        "kn_chan_try_recv",
        &[types::I64, types::I64],
        Some(types::I32),
    ),
    ("kn_chan_close", &[types::I64], None),
    ("kn_chan_len", &[types::I64], Some(types::I64)),
    ("kn_chan_cap", &[types::I64], Some(types::I64)),
    ("kn_chan_clone_end", &[types::I64, types::I32], None),
    ("kn_chan_drop_end", &[types::I64, types::I32], None),
    (
        "kn_select",
        &[types::I64, types::I64, types::I64],
        Some(types::I64),
    ),
    (
        "kn_slice_check",
        &[
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::I64,
            types::I64,
        ],
        Some(types::I64),
    ),
    ("kn_memmove", &[types::I64, types::I64, types::I64], None),
    ("kn_maybe_yield", &[], None),
    ("kn_round", &[types::F64], Some(types::F64)),
    ("kn_powi", &[types::F64, types::I32], Some(types::F64)),
    ("kn_fmod", &[types::F64, types::F64], Some(types::F64)),
];

/// Which code generator turns the program into machine code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Backend {
    /// Cranelift: fast to compile; the default for debug builds.
    #[default]
    Cranelift,
    /// LLVM (`O3`): slower to compile, faster code. Only in a `kiln` built
    /// with the `llvm` feature; see [`llvm_available`].
    Llvm,
}

/// Whether this build of the compiler includes the LLVM backend.
pub fn llvm_available() -> bool {
    cfg!(feature = "llvm")
}

/// Build settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Release build: integer arithmetic wraps on overflow instead of panicking.
    pub release: bool,
    /// The code generator.
    pub backend: Backend,
}

/// Functions and data recorded for the LLVM backend instead of being
/// compiled by Cranelift.
#[derive(Default)]
pub(crate) struct Captured {
    pub(crate) funcs: Vec<(FuncId, cranelift_codegen::ir::Function, Hints)>,
    pub(crate) data: Vec<(DataId, DataDescription)>,
}

/// Facts from the Kiln types about a function's parameters, for the LLVM
/// backend (see `kiln_llvm::FnHints`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Hints {
    /// (index, fresh slot, size, align) of pointer parameters.
    pub(crate) params: Vec<(usize, bool, u32, u32)>,
    /// (slot parameter, mutable, size, align) of reference arguments.
    pub(crate) refs: Vec<(usize, bool, u32, u32)>,
    pub(crate) inline: bool,
}

/// Compiles a type-checked, borrow-checked program to an object file.
///
/// Returns the object file bytes, or diagnostics for constructs the code
/// generator doesn't support yet.
pub fn compile(typed: Typed<'_, '_>, opts: Options) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").expect("is_pic");
    flags.set("opt_level", "speed").expect("opt_level");
    // Large stack frames must touch each guard page (required on Windows);
    // inline probes avoid needing a `__cranelift_probestack` symbol.
    flags
        .set("enable_probestack", "true")
        .expect("enable_probestack");
    flags
        .set("probestack_strategy", "inline")
        .expect("probestack_strategy");
    let isa = cranelift_native::builder()
        .map_err(|e| {
            vec![Diagnostic::error(
                format!("unsupported host for code generation: {e}"),
                Span::new(0, 0),
            )]
        })?
        .finish(settings::Flags::new(flags))
        .map_err(|e| {
            vec![Diagnostic::error(
                format!("code generator setup failed: {e}"),
                Span::new(0, 0),
            )]
        })?;
    let builder = ObjectBuilder::new(isa, "kiln", default_libcall_names()).map_err(|e| {
        vec![Diagnostic::error(
            format!("code generator setup failed: {e}"),
            Span::new(0, 0),
        )]
    })?;
    if opts.backend == Backend::Llvm && !llvm_available() {
        return Err(vec![Diagnostic::error(
            "this `kiln` was built without the LLVM backend (rebuild with `--features llvm`, see docs/LLVM.md)",
            Span::new(0, 0),
        )]);
    }
    let r = typed.resolved();
    let mut cg = Codegen {
        checked: !opts.release,
        typed,
        r,
        module: ObjectModule::new(builder),
        capture: (opts.backend == Backend::Llvm).then(Captured::default),
        freeze_cache: HashMap::new(),
        bodies: HashMap::new(),
        instances: HashMap::new(),
        queue: VecDeque::new(),
        layouts: HashMap::new(),
        needs_drop_cache: HashMap::new(),
        drop_glue: HashMap::new(),
        eq_glue: HashMap::new(),
        cmp_glue: HashMap::new(),
        hash_glue: HashMap::new(),
        debug_glue: HashMap::new(),
        clone_glue: HashMap::new(),
        vtables: HashMap::new(),
        shims: HashMap::new(),
        strings: HashMap::new(),
        statics: Default::default(),
        rt: HashMap::new(),
        externs: HashMap::new(),
        errors: vec![],
        counter: 0,
        span: Span::new(0, 0),
        entry_display: None,
    };
    cg.declare_runtime();
    let Some(main_idx) = (0..r.fns.len()).find(|&i| {
        let f = &r.fns[i];
        !f.is_prelude && f.impl_idx.is_none() && f.trait_def.is_none() && f.decl.name.name == "main"
    }) else {
        return Err(vec![Diagnostic::error(
            "no `main` function to build",
            Span::new(0, 0),
        )]);
    };
    let user_main = cg.instance(main_idx, 0, HashMap::new(), Abi::Plain);
    // How a `main` that returns `Err(e)` prints `e`.
    cg.entry_display = cg.main_error_display(main_idx);
    cg.drain();
    // Compile the initializers of the statics in use (which may use more).
    let mut inits: Vec<(DataId, FuncId)> = Vec::new();
    let mut done = std::collections::HashSet::new();
    loop {
        let todo: Vec<_> = cg
            .statics
            .iter()
            .filter(|(d, _)| !done.contains(*d))
            .map(|(d, v)| (*d, *v))
            .collect();
        if todo.is_empty() {
            break;
        }
        for (d, (data, fi)) in todo {
            done.insert(d);
            let f = cg.instance(fi, 0, HashMap::new(), Abi::Plain);
            inits.push((data, f));
        }
        cg.drain();
    }
    // Declaration order (statics are keyed by definition).
    let order: Vec<DataId> = cg.statics.values().map(|v| v.0).collect();
    inits.sort_by_key(|(d, _)| order.iter().position(|x| x == d));
    if cg.errors.is_empty() {
        cg.define_entry(main_idx, user_main, &inits);
    }
    if !cg.errors.is_empty() {
        let mut errs = std::mem::take(&mut cg.errors);
        errs.dedup_by(|a, b| a.message == b.message && a.span == b.span);
        return Err(errs);
    }
    if let Some(cap) = cg.capture.take() {
        return emit_llvm(&cg.module, cap);
    }
    let product = cg.module.finish();
    product.emit().map_err(|e| {
        vec![Diagnostic::error(
            format!("could not write object file: {e}"),
            Span::new(0, 0),
        )]
    })
}

#[cfg(feature = "llvm")]
fn emit_llvm(module: &ObjectModule, cap: Captured) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let funcs: Vec<kiln_llvm::Func> = cap
        .funcs
        .into_iter()
        .map(|(id, func, h)| kiln_llvm::Func {
            id,
            func,
            hints: kiln_llvm::FnHints {
                params: h
                    .params
                    .iter()
                    .map(|&(index, noalias, size, align)| kiln_llvm::ParamHint {
                        index,
                        noalias,
                        size,
                        align,
                    })
                    .collect(),
                refs: h
                    .refs
                    .iter()
                    .map(|&(param, mutable, size, align)| kiln_llvm::RefHint {
                        param,
                        mutable,
                        size,
                        align,
                    })
                    .collect(),
                inline: h.inline,
            },
        })
        .collect();
    let settings = kiln_llvm::Settings {
        cpu: std::env::var("KILN_TARGET_CPU").ok(),
        dump_ir: std::env::var_os("KILN_LLVM_DUMP").map(Into::into),
    };
    kiln_llvm::emit(module.declarations(), &funcs, &cap.data, &settings)
        .map_err(|e| vec![Diagnostic::error(e, Span::new(0, 0))])
}

#[cfg(not(feature = "llvm"))]
fn emit_llvm(_: &ObjectModule, _: Captured) -> Result<Vec<u8>, Vec<Diagnostic>> {
    unreachable!("checked in `compile`")
}

impl Codegen<'_, '_> {
    /// Defines a function: compiles it with Cranelift, or records it for LLVM.
    pub(crate) fn def_func(
        &mut self,
        id: FuncId,
        ctx: &mut cranelift_codegen::Context,
        hints: Hints,
    ) -> Result<(), String> {
        match &mut self.capture {
            Some(c) => {
                c.funcs.push((id, ctx.func.clone(), hints));
                Ok(())
            }
            None => self
                .module
                .define_function(id, ctx)
                .map_err(|e| format!("{e:?}")),
        }
    }

    /// Defines a data object (see [`Self::def_func`]).
    pub(crate) fn def_data(&mut self, id: DataId, d: &DataDescription) -> Result<(), String> {
        match &mut self.capture {
            Some(c) => {
                c.data.push((id, d.clone()));
                Ok(())
            }
            None => self.module.define_data(id, d).map_err(|e| e.to_string()),
        }
    }

    /// Records an error at the current item.
    pub(crate) fn fatal(&mut self, msg: impl Into<String>) {
        let d = Diagnostic::error(msg, self.span);
        self.errors.push(d);
    }

    pub(crate) fn error_at(&mut self, msg: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::error(msg, span));
    }

    fn declare_runtime(&mut self) {
        for (name, params, ret) in RUNTIME {
            let mut sig = self.module.make_signature();
            sig.params.extend(params.iter().map(|t| AbiParam::new(*t)));
            sig.returns.extend(ret.iter().map(|t| AbiParam::new(*t)));
            let id = self
                .module
                .declare_function(name, Linkage::Import, &sig)
                .expect("declare runtime");
            self.rt.insert(name, id);
        }
    }

    /// A signature with `n` pointer parameters and no results.
    pub(crate) fn ptr_sig(&self, n: usize) -> Signature {
        let mut sig = self.module.make_signature();
        sig.params.extend((0..n).map(|_| AbiParam::new(types::I64)));
        sig
    }

    fn fresh_name(&mut self, base: &str) -> String {
        self.counter += 1;
        let clean: String = base
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("kn{}_{}", self.counter, clean)
    }

    /// The MIR bodies of function `i` (lowered on first use).
    pub(crate) fn bodies(&mut self, i: usize) -> &Vec<Body> {
        if !self.bodies.contains_key(&i) {
            let b = kiln_mir::lower_fn(&mut self.typed, i);
            return self.bodies.entry(i).or_insert(b);
        }
        &self.bodies[&i]
    }

    /// Declares (and queues for definition) the instance of body `body` of
    /// function `fn_idx` with generic substitution `substs`.
    pub(crate) fn instance(
        &mut self,
        fn_idx: usize,
        body: usize,
        substs: HashMap<String, Ty>,
        abi: Abi,
    ) -> FuncId {
        let mut keyv: Vec<(String, String)> = substs
            .iter()
            .map(|(k, v)| (k.clone(), format!("{v:?}")))
            .collect();
        keyv.sort();
        let key = format!("{fn_idx}/{body}/{abi:?}/{keyv:?}");
        if let Some(id) = self.instances.get(&key) {
            return *id;
        }
        let nargs = self
            .bodies(fn_idx)
            .get(body)
            .map(|b| b.arg_count)
            .unwrap_or(0);
        let extra = if abi == Abi::Closure { 2 } else { 1 };
        let sig = self.ptr_sig(nargs + extra);
        let base = self
            .bodies(fn_idx)
            .get(body)
            .map(|b| b.name.clone())
            .unwrap_or_default();
        let name = self.fresh_name(&base);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare fn");
        self.instances.insert(key, id);
        self.queue.push_back(Work::Fn {
            id,
            fn_idx,
            body,
            substs,
            abi,
        });
        id
    }

    /// A shim with the closure ABI around a plain function (for fn items used as values).
    pub(crate) fn shim(&mut self, target: FuncId, nargs: usize) -> FuncId {
        if let Some(id) = self.shims.get(&target) {
            return *id;
        }
        let sig = self.ptr_sig(nargs + 2);
        let name = self.fresh_name("shim");
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .expect("declare shim");
        self.shims.insert(target, id);
        self.queue.push_back(Work::Shim { id, target, nargs });
        id
    }

    /// The storage for static `d`: zeroed until its initializer runs.
    pub(crate) fn static_data(&mut self, d: kiln_resolve::DefId) -> DataId {
        if let Some((id, _)) = self.statics.get(&d) {
            return *id;
        }
        let fi = self
            .r
            .statics
            .iter()
            .find(|(sd, _)| *sd == d)
            .map(|(_, i)| *i)
            .expect("static without initializer");
        let (_, ty) = self.typed.signature(fi);
        let l = self.layout(&ty);
        let name = self.fresh_name(&format!("static_{}", self.r.def(d).name));
        let id = self
            .module
            .declare_data(&name, Linkage::Local, true, false)
            .expect("declare static");
        let mut desc = DataDescription::new();
        desc.define_zeroinit(l.size.max(1) as usize);
        desc.set_align(l.align.max(1) as u64);
        self.def_data(id, &desc).expect("define static");
        self.statics.insert(d, (id, fi));
        id
    }

    /// A string literal's bytes in the data section.
    /// Read-only data for a byte string literal.
    pub(crate) fn bytes_data(&mut self, b: &[u8]) -> DataId {
        let name = self.fresh_name("bytes");
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("declare data");
        let mut d = DataDescription::new();
        let mut bytes = b.to_vec();
        bytes.push(0);
        d.define(bytes.into_boxed_slice());
        self.def_data(id, &d).expect("define data");
        id
    }

    pub(crate) fn string_data(&mut self, s: &str) -> DataId {
        if let Some(id) = self.strings.get(s) {
            return *id;
        }
        let name = self.fresh_name("str");
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("declare data");
        let mut d = DataDescription::new();
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        d.define(bytes.into_boxed_slice());
        self.def_data(id, &d).expect("define data");
        self.strings.insert(s.to_string(), id);
        id
    }

    fn drain(&mut self) {
        while let Some(w) = self.queue.pop_front() {
            if self.errors.len() > 20 {
                return;
            }
            match w {
                Work::Fn {
                    id,
                    fn_idx,
                    body,
                    substs,
                    abi,
                } => self.define_fn(id, fn_idx, body, substs, abi),
                Work::DropGlue { id, ty } => self.define_drop_glue(id, &ty),
                Work::EqGlue { id, ty } => self.define_eq_glue(id, &ty),
                Work::CmpGlue { id, ty } => self.define_cmp_glue(id, &ty),
                Work::HashGlue { id, ty } => self.define_hash_glue(id, &ty),
                Work::DebugGlue { id, ty } => self.define_debug_glue(id, &ty),
                Work::CloneGlue { id, ty } => self.define_clone_glue(id, &ty),
                Work::Shim { id, target, nargs } => self.define_shim(id, target, nargs),
            }
        }
    }
}

/// Why a build failed.
#[derive(Debug)]
pub enum BuildError {
    /// Errors in the program (or constructs codegen doesn't support), with source spans.
    Diagnostics(Vec<Diagnostic>),
    /// I/O or linker failures.
    Other(String),
}

/// Checks `src` (parse, resolve, type-check, borrow-check), generates code, and
/// links an executable at `out`.
pub fn build_source(src: &str, out: &std::path::Path, opts: Options) -> Result<(), BuildError> {
    build_parsed(kiln_parser::parse(src), out, opts)
}

/// Builds the test executable for `src` (see [`kiln_parser::test_harness`]):
/// running it with a test's name runs that test. Returns the tests found.
pub fn build_tests(
    src: &str,
    out: &std::path::Path,
    opts: Options,
) -> Result<Vec<kiln_parser::TestCase>, BuildError> {
    build_tests_parsed(kiln_parser::parse(src), out, opts)
}

/// [`build_tests`] for an already-parsed program.
pub fn build_tests_parsed(
    mut parsed: kiln_parser::Parse,
    out: &std::path::Path,
    opts: Options,
) -> Result<Vec<kiln_parser::TestCase>, BuildError> {
    if !parsed.errors.is_empty() {
        return Err(BuildError::Diagnostics(parsed.errors));
    }
    let (tests, errs) = kiln_parser::test_harness(&mut parsed.file);
    if !errs.is_empty() {
        return Err(BuildError::Diagnostics(errs));
    }
    build_parsed(parsed, out, opts)?;
    Ok(tests)
}

/// Builds an already-parsed program (a package loaded with
/// [`kiln_parser::package::load`]).
pub fn build_parsed(
    parsed: kiln_parser::Parse,
    out: &std::path::Path,
    opts: Options,
) -> Result<(), BuildError> {
    if !parsed.errors.is_empty() {
        return Err(BuildError::Diagnostics(parsed.errors));
    }
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    if !resolved.errors.is_empty() {
        return Err(BuildError::Diagnostics(resolved.errors));
    }
    let mut typed = kiln_typeck::typeck(&resolved);
    if !typed.errors().is_empty() {
        return Err(BuildError::Diagnostics(typed.errors().to_vec()));
    }
    let errs = kiln_borrowck::check(&mut typed);
    if !errs.is_empty() {
        return Err(BuildError::Diagnostics(errs));
    }
    let obj = compile(typed, opts).map_err(BuildError::Diagnostics)?;
    let dir = std::env::temp_dir().join(format!("kiln-build-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| BuildError::Other(format!("cannot create {}: {e}", dir.display())))?;
    let obj_path = dir.join(if cfg!(windows) { "main.obj" } else { "main.o" });
    std::fs::write(&obj_path, obj)
        .map_err(|e| BuildError::Other(format!("cannot write object file: {e}")))?;
    let res = link::link(&obj_path, out).map_err(BuildError::Other);
    let _ = std::fs::remove_dir_all(&dir);
    res
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    t ^ N.fetch_add(1, Ordering::Relaxed)
}
