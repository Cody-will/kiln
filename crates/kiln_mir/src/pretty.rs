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
                Proj::Index => s.push_str("[_]"),
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
            Operand::Const => f.write_str("const"),
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
            Rvalue::Aggregate(ops) => write!(f, "aggregate({})", list(ops)),
            Rvalue::Call { args, flows } => write!(f, "call({}) flows {flows:?}", list(args)),
            Rvalue::Pure(ops) => write!(f, "op({})", list(ops)),
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
            writeln!(f, "  bb{i}:")?;
            for s in &b.stmts {
                match &s.kind {
                    StmtKind::Assign(p, rv) => writeln!(f, "    {p} = {rv}")?,
                    StmtKind::Read(p) => writeln!(f, "    read {p}")?,
                    StmtKind::Eval(rv) => writeln!(f, "    eval {rv}")?,
                    StmtKind::StorageDead(l) => writeln!(f, "    dead _{}", l.0)?,
                    StmtKind::Spawn {
                        kind,
                        operands,
                        captures,
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
                Terminator::Unreachable => "unreachable".into(),
            };
            writeln!(f, "    {t}")?;
        }
        writeln!(f, "}}")
    }
}
