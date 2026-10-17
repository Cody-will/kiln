//! A canonical dump of MIR, used by the differential test of the
//! self-hosted MIR lowering (`compiler/src/mir.kn`).
//!
//! Unlike `kiln mir` (the `Display` impls in `pretty.rs`), it prints every
//! field: local declarations with spans and sources, statement and
//! terminator spans, callee substitutions, captures, return flows, lowering
//! errors, unsupported constructs, and bounded loops. Types print as in
//! `kiln typeck --dump` (definition ids, `'T` for parameters), strings with
//! non-printable bytes escaped, and characters by code point, so nothing
//! depends on how either language formats `Debug`.

use crate::ir::*;
use kiln_typeck::ty::Ty;
use std::fmt::Write;

fn prim(p: kiln_resolve::PrimTy) -> String {
    format!("{p:?}").to_lowercase()
}

/// Canonical text of a type.
pub fn ty(t: &Ty) -> String {
    let all = |ts: &[Ty]| ts.iter().map(ty).collect::<Vec<_>>().join(", ");
    match t {
        Ty::Prim(p) => prim(*p),
        Ty::Unit => "()".into(),
        Ty::Never => "!".into(),
        Ty::Tuple(ts) => format!("({})", all(ts)),
        Ty::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, ty(x)),
        Ty::Ptr(m, x) => format!("*{} {}", if *m { "mut" } else { "const" }, ty(x)),
        Ty::Array(x, n) => format!("[{}; {n}]", ty(x)),
        Ty::Slice(x) => format!("[{}]", ty(x)),
        Ty::Adt(d, args) => format!("d{}<{}>", d.0, all(args)),
        Ty::Fn(ps, r, m) => format!("fun{}({}) {}", if *m { " mut" } else { "" }, all(ps), ty(r)),
        Ty::Param(n) => format!("'{n}"),
        Ty::Dyn(ds) => format!(
            "dyn {}",
            ds.iter()
                .map(|d| format!("d{}", d.0))
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        Ty::ChanSend(x) => format!("chan<- {}", ty(x)),
        Ty::ChanRecv(x) => format!("<-chan {}", ty(x)),
        Ty::Var(_) => "?".into(),
        Ty::Unknown => "{unknown}".into(),
    }
}

/// Bytes with anything outside printable ASCII (and `\`, `"`) as `\xNN`.
pub fn esc(b: &[u8]) -> String {
    let mut o = String::new();
    for &c in b {
        if (32..127).contains(&c) && c != b'\\' && c != b'"' {
            o.push(c as char);
        } else {
            let _ = write!(o, "\\x{c:02x}");
        }
    }
    o
}

fn sp(s: kiln_lexer::Span) -> String {
    format!("{}..{}", s.start, s.end)
}

fn place(p: &Place) -> String {
    let mut s = format!("_{}", p.local.0);
    for x in &p.proj {
        match x {
            Proj::Deref(k) => {
                let k = match k {
                    DerefKindKey::Shared => "&",
                    DerefKindKey::Mut => "&mut",
                    DerefKindKey::Ptr => "ptr",
                    DerefKindKey::Box => "box",
                };
                s = format!("(*{k} {s})");
            }
            Proj::Field(n) => {
                let _ = write!(s, ".{n}");
            }
            Proj::Index(l) => {
                let _ = write!(s, "[_{}]", l.0);
            }
        }
    }
    s
}

fn substs(v: &[(String, Ty)]) -> String {
    v.iter()
        .map(|(n, t)| format!("{n}={}", ty(t)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn operand(o: &Operand) -> String {
    match o {
        Operand::Copy(p) => format!("copy {}", place(p)),
        Operand::Move(p) => format!("move {}", place(p)),
        Operand::Const(c) => {
            let k = match &c.kind {
                ConstKind::Int(v) => format!("{v}"),
                ConstKind::Float(v) => format!("{v:?}"),
                ConstKind::Bool(v) => format!("{v}"),
                ConstKind::Char(v) => format!("c{}", *v as u32),
                ConstKind::Str(v) => format!("\"{}\"", esc(v.as_bytes())),
                ConstKind::Bytes(v) => format!("b\"{}\"", esc(v)),
                ConstKind::Unit => "()".into(),
                ConstKind::Fn(i, s) => format!("fn#{i}[{}]", substs(s)),
            };
            format!("const {k}: {}", ty(&c.ty))
        }
    }
}

fn list(ops: &[Operand]) -> String {
    ops.iter().map(operand).collect::<Vec<_>>().join(", ")
}

fn spec(s: &Option<kiln_parser::ast::FmtSpec>) -> String {
    let Some(s) = s else { return "-".into() };
    let opt = |x: Option<usize>| x.map_or("-".to_string(), |v| v.to_string());
    format!(
        "fill={} align={} plus={} alt={} zero={} width={} precision={} kind={:?}",
        s.fill.map_or("-".to_string(), |c| format!("c{}", c as u32)),
        s.align.map_or("-".to_string(), |a| format!("{a:?}")),
        s.plus,
        s.alt,
        s.zero,
        opt(s.width),
        opt(s.precision),
        s.kind
    )
}

fn op(o: &Op) -> String {
    match o {
        Op::Bin(b) => format!("{b:?}"),
        Op::Neg => "Neg".into(),
        Op::Not => "Not".into(),
        Op::Cast(t) => format!("Cast {}", ty(t)),
        Op::Discriminant => "Discriminant".into(),
        Op::Print { newline, stderr } => format!("Print newline={newline} stderr={stderr}"),
        Op::Interp(segs) => {
            let s: Vec<String> = segs
                .iter()
                .map(|g| match g {
                    InterpSeg::Text(t) => format!("text \"{}\"", esc(t.as_bytes())),
                    InterpSeg::Arg(i, sp) => format!("arg {i} {}", spec(sp)),
                })
                .collect();
            format!("Interp [{}]", s.join("; "))
        }
        Op::Touch => "Touch".into(),
        Op::Unsupported(s) => format!("Unsupported \"{}\"", esc(s.as_bytes())),
    }
}

fn rvalue(r: &Rvalue) -> String {
    match r {
        Rvalue::Use(o) => format!("use {}", operand(o)),
        Rvalue::Ref { mutable, place: p } => {
            format!("&{}{}", if *mutable { "mut " } else { "" }, place(p))
        }
        Rvalue::Aggregate(k, ops) => {
            let k = match k {
                AggKind::Tuple => "tuple".to_string(),
                AggKind::Array => "array".to_string(),
                AggKind::ArrayRepeat(n) => format!("array_repeat[{n}]"),
                AggKind::Struct(d, fs) => format!("struct d{}{{{}}}", d.0, fs.join(",")),
                AggKind::Variant { def, index, fields } => {
                    format!("variant d{}#{index}{{{}}}", def.0, fields.join(","))
                }
                AggKind::Range {
                    has_start,
                    has_end,
                    inclusive,
                } => format!("range[{has_start} {has_end} {inclusive}]"),
                AggKind::Closure(s) => format!("closure[{}]", sp(*s)),
            };
            format!("{k}({})", list(ops))
        }
        Rvalue::Call {
            callee,
            args,
            flows,
        } => {
            let c = match callee {
                Callee::Fn(i, s) => format!("fn#{i}[{}]", substs(s)),
                Callee::Value => "value".to_string(),
                Callee::Intrinsic(n) => format!("intrinsic \"{}\"", esc(n.as_bytes())),
            };
            format!("call {c}({}) flows {flows:?}", list(args))
        }
        Rvalue::Coerce(o, t) => format!("coerce({} to {})", operand(o), ty(t)),
        Rvalue::Pure(o, ops) => format!("op {}({})", op(o), list(ops)),
    }
}

fn mode(m: CaptureMode) -> &'static str {
    match m {
        CaptureMode::Ref => "ref",
        CaptureMode::MutRef => "mut",
        CaptureMode::Move => "move",
    }
}

/// Canonical text of one body.
pub fn body(b: &Body) -> String {
    let mut o = String::new();
    let kind = match b.kind {
        BodyKind::Fn => "fn",
        BodyKind::Closure => "closure",
        BodyKind::GoBlock => "spawn",
    };
    let rf = b
        .return_flows
        .as_ref()
        .map_or("-".to_string(), |f| format!("{f:?}"));
    let _ = writeln!(
        o,
        "body {} {kind} {} args={} return_flows={rf}",
        b.name,
        sp(b.span),
        b.arg_count
    );
    for (i, l) in b.locals.iter().enumerate() {
        let k = match l.kind {
            LocalKind::Return => "return",
            LocalKind::Arg => "arg",
            LocalKind::User => "user",
            LocalKind::Temp => "temp",
            LocalKind::Upvar => "upvar",
        };
        let _ = writeln!(
            o,
            "  _{i} {k} {} {} {} {}{}",
            ty(&l.ty),
            l.name.as_deref().unwrap_or("-"),
            sp(l.span),
            if l.mutable { "mut" } else { "imm" },
            l.source.map_or(String::new(), |s| format!(" l{}", s.0))
        );
    }
    for (i, blk) in b.blocks.iter().enumerate() {
        let bounded = if b.bounded_loops.iter().any(|h| h.0 as usize == i) {
            " bounded"
        } else {
            ""
        };
        let _ = writeln!(o, "  bb{i}{bounded}");
        for s in &blk.stmts {
            let t = match &s.kind {
                StmtKind::Assign(p, rv) => format!("{} = {}", place(p), rvalue(rv)),
                StmtKind::Read(p) => format!("read {}", place(p)),
                StmtKind::Eval(rv) => format!("eval {}", rvalue(rv)),
                StmtKind::StorageDead(l) => format!("dead _{}", l.0),
                StmtKind::DeferPush(n, r) => {
                    format!(
                        "defer_push {n} {}",
                        r.map_or("-".into(), |r| format!("_{}", r.0))
                    )
                }
                StmtKind::DeferLoad(r) => format!("defer_load _{}", r.0),
                StmtKind::Spawn {
                    kind,
                    operands,
                    captures,
                    body,
                } => {
                    let k = match kind {
                        SpawnKind::GoCall => "call".to_string(),
                        SpawnKind::GoBlock { is_move } => format!("block move={is_move}"),
                        SpawnKind::Scoped => "scoped".to_string(),
                    };
                    let caps: Vec<String> = captures
                        .iter()
                        .map(|c| {
                            format!("{}:{}:{}:{}", c.name, ty(&c.ty), mode(c.mode), sp(c.span))
                        })
                        .collect();
                    format!(
                        "spawn {k} ({}) captures [{}] body {}",
                        list(operands),
                        caps.join(", "),
                        sp(*body)
                    )
                }
            };
            let _ = writeln!(o, "    {t} @{}", sp(s.span));
        }
        let t = match &blk.term {
            Terminator::Goto(t) => format!("goto bb{}", t.0),
            Terminator::SwitchBool { cond, t, f } => {
                format!("if {} bb{} bb{}", operand(cond), t.0, f.0)
            }
            Terminator::Branch(ts) => format!(
                "branch [{}]",
                ts.iter()
                    .map(|t| format!("bb{}", t.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Terminator::Return => "return".into(),
            Terminator::Unreachable => "unreachable".into(),
            Terminator::DeferDispatch { targets, done } => format!(
                "defer_dispatch [{}] bb{}",
                targets
                    .iter()
                    .map(|t| t.map_or("-".to_string(), |t| format!("bb{}", t.0)))
                    .collect::<Vec<_>>()
                    .join(", "),
                done.0
            ),
        };
        let _ = writeln!(o, "    {t} @{}", sp(blk.term_span));
    }
    for (l, m) in &b.captures {
        let _ = writeln!(o, "  capture _{} {}", l.0, mode(*m));
    }
    for (m, s) in &b.errors {
        let _ = writeln!(o, "  error {}: {m}", sp(*s));
    }
    for (m, s) in &b.unsupported {
        let _ = writeln!(o, "  unsupported {}: {m}", sp(*s));
    }
    o
}

/// Parses, resolves, type-checks, and lowers `src`, printing every body of
/// every user function. Stops after the first phase with errors, printing
/// the number of errors.
pub fn dump(src: &str) -> String {
    let parsed = kiln_parser::parse(src);
    if !parsed.errors.is_empty() {
        return format!("parse errors {}\n", parsed.errors.len());
    }
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    if !resolved.errors.is_empty() {
        return format!("resolve errors {}\n", resolved.errors.len());
    }
    let mut typed = kiln_typeck::typeck(&resolved);
    if !typed.errors().is_empty() {
        return format!("type errors {}\n", typed.errors().len());
    }
    let mut o = String::new();
    for (i, bodies) in crate::lower_all(&mut typed) {
        let _ = writeln!(o, "fn f{i}");
        for b in &bodies {
            o.push_str(&body(b));
        }
    }
    o
}
