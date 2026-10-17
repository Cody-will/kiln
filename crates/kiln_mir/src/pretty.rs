//! A textual dump of MIR (`kiln mir <file>`).

use crate::ir::*;
use std::fmt::{self, Write};

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = format!("_{}", self.local.0);
        for p in &self.proj {
            match p {
                Proj::Deref(_) => s = format!("(*{s})"),
                Proj::Field(n) => {
                    let _ = write!(s, ".{n}");
                }
                Proj::Index(l) => {
                    let _ = write!(s, "[_{}]", l.0);
                }
            }
        }
        f.write_str(&s)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Copy(p) => write!(f, "copy {p}"),
            Operand::Move(p) => write!(f, "move {p}"),
            Operand::Const(c) => match &c.kind {
                ConstKind::Int(v) => write!(f, "const {v}"),
                ConstKind::Float(v) => write!(f, "const {v:?}"),
                ConstKind::Bool(v) => write!(f, "const {v}"),
                ConstKind::Char(v) => write!(f, "const {v:?}"),
                ConstKind::Str(v) => write!(f, "const {v:?}"),
                ConstKind::Bytes(v) => write!(f, "const b{:?}", String::from_utf8_lossy(v)),
                ConstKind::Unit => f.write_str("const ()"),
                ConstKind::Fn(i, _) => write!(f, "const fn#{i}"),
            },
        }
    }
}

fn list(ops: &[Operand]) -> String {
    ops.iter()
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(o) => write!(f, "{o}"),
            Rvalue::Ref { mutable, place } => {
                write!(f, "&{}{place}", if *mutable { "mut " } else { "" })
            }
            Rvalue::Aggregate(k, ops) => {
                let k = match k {
                    AggKind::Tuple => "tuple".to_string(),
                    AggKind::Array => "array".to_string(),
                    AggKind::ArrayRepeat(n) => format!("array_repeat[{n}]"),
                    AggKind::Struct(_, fs) => format!("struct{{{}}}", fs.join(",")),
                    AggKind::Variant { index, .. } => format!("variant#{index}"),
                    AggKind::Range { .. } => "range".to_string(),
                    AggKind::Closure(_) => "closure".to_string(),
                };
                write!(f, "{k}({})", list(ops))
            }
            Rvalue::Call {
                callee,
                args,
                flows,
            } => {
                let c = match callee {
                    Callee::Fn(i, _) => format!("fn#{i}"),
                    Callee::Value => "value".to_string(),
                    Callee::Intrinsic(n) => n.clone(),
                };
                write!(f, "call {c}({}) flows {flows:?}", list(args))
            }
            Rvalue::Coerce(o, t) => write!(f, "coerce({o} to {t:?})"),
            Rvalue::Pure(op, ops) => {
                let o = match op {
                    Op::Bin(b) => format!("{b:?}"),
                    Op::Interp(_) => "interp".into(),
                    o => format!("{o:?}"),
                };
                write!(f, "op {o}({})", list(ops))
            }
        }
    }
}

impl fmt::Display for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} {:?} (args: {}) {{",
            self.name, self.kind, self.arg_count
        )?;
        for (i, l) in self.locals.iter().enumerate() {
            writeln!(
                f,
                "    let _{i}: {:?} // {:?}{}",
                l.kind,
                l.ty,
                l.name
                    .as_ref()
                    .map(|n| format!(" `{n}`"))
                    .unwrap_or_default()
            )?;
        }
        for (i, b) in self.blocks.iter().enumerate() {
            if self.bounded_loops.iter().any(|h| h.0 as usize == i) {
                writeln!(f, "  bb{i}: // bounded loop")?;
            } else {
                writeln!(f, "  bb{i}:")?;
            }
            for s in &b.stmts {
                match &s.kind {
                    StmtKind::Assign(p, rv) => writeln!(f, "    {p} = {rv}")?,
                    StmtKind::Read(p) => writeln!(f, "    read {p}")?,
                    StmtKind::Eval(rv) => writeln!(f, "    eval {rv}")?,
                    StmtKind::StorageDead(l) => writeln!(f, "    dead _{}", l.0)?,
                    StmtKind::DeferPush(n, None) => writeln!(f, "    defer_push {n}")?,
                    StmtKind::DeferPush(n, Some(r)) => {
                        writeln!(f, "    defer_push {n}(move _{})", r.0)?
                    }
                    StmtKind::DeferLoad(r) => writeln!(f, "    _{} = defer_load", r.0)?,
                    StmtKind::Spawn {
                        kind,
                        operands,
                        captures,
                        ..
                    } => {
                        let caps: Vec<String> = captures
                            .iter()
                            .map(|c| format!("{}:{:?}", c.name, c.mode))
                            .collect();
                        writeln!(
                            f,
                            "    spawn {kind:?} ({}) captures [{}]",
                            list(operands),
                            caps.join(", ")
                        )?
                    }
                }
            }
            let t = match &b.term {
                Terminator::Goto(t) => format!("goto bb{}", t.0),
                Terminator::SwitchBool { cond, t, f } => {
                    format!("if {cond} goto bb{} else bb{}", t.0, f.0)
                }
                Terminator::Branch(ts) => {
                    format!(
                        "branch [{}]",
                        ts.iter()
                            .map(|t| format!("bb{}", t.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
                Terminator::Return => "return".into(),
                Terminator::DeferDispatch { targets, done } => format!(
                    "defer_dispatch [{}] else bb{}",
                    targets
                        .iter()
                        .map(|t| t.map(|t| format!("bb{}", t.0)).unwrap_or("-".into()))
                        .collect::<Vec<_>>()
                        .join(", "),
                    done.0
                ),
                Terminator::Unreachable => "unreachable".into(),
            };
            writeln!(f, "    {t}")?;
        }
        writeln!(f, "}}")
    }
}
