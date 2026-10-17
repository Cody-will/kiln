//! A canonical dump of borrow-checker diagnostics, used by the differential
//! test of the self-hosted borrow checker (`compiler/src/borrowck.kn`).

use kiln_lexer::Diagnostic;
use std::fmt::Write;

/// Prints each diagnostic: `error START..END: message`, then its notes
/// (`note START..END: msg` or `note -: msg`) and help line.
pub fn diagnostics(ds: &[Diagnostic]) -> String {
    let mut o = String::new();
    for d in ds {
        let _ = writeln!(o, "error {}..{}: {}", d.span.start, d.span.end, d.message);
        for (m, s) in &d.notes {
            match s {
                Some(s) => {
                    let _ = writeln!(o, "  note {}..{}: {m}", s.start, s.end);
                }
                None => {
                    let _ = writeln!(o, "  note -: {m}");
                }
            }
        }
        if let Some(h) = &d.help {
            let _ = writeln!(o, "  help: {h}");
        }
    }
    o
}

/// Parses, resolves, type-checks, and borrow-checks `src`. Stops after the
/// first earlier phase with errors, printing their number.
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
    diagnostics(&crate::check(&mut typed))
}
