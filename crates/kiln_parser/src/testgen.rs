//! Test harness generation for `kiln test`.
//!
//! [`test_harness`] finds `#[test]` functions (at the top level and in inline
//! modules), drops the file's own `main`, and appends a generated `main` that
//! runs the one test named by its first argument. The test driver runs the
//! executable once per test, so a panic (which exits the process) fails just
//! that test.

use crate::ast::{File, Item, ItemKind, Visibility};
use kiln_lexer::Diagnostic;

/// One discovered test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    /// Path of the function from the file root, e.g. `parse::empty_input`.
    pub name: String,
    /// Marked `#[ignore]`: skipped unless asked for.
    pub ignore: bool,
    /// Marked `#[should_panic]`; the inner value is the `expected = "..."`
    /// text the panic message must contain, if given.
    pub should_panic: Option<Option<String>>,
}

/// Rewrites `file` into a test program; returns the tests found and any
/// errors in their declarations.
pub fn test_harness(file: &mut File) -> (Vec<TestCase>, Vec<Diagnostic>) {
    let mut tests = vec![];
    let mut calls = vec![];
    let mut errors = vec![];
    collect(&mut file.items, "", &mut tests, &mut calls, &mut errors);
    file.items
        .retain(|it| !matches!(&it.kind, ItemKind::Fn(f) if f.name.name == "main"));
    let mut src = String::from("fn main() {\n\targs := std::os::args()\n\tif args.len() < 2 {\n\t\tstd::os::exit(2)\n\t}\n\twhich := &args[1]\n");
    for (t, returns) in tests.iter().zip(&calls) {
        if *returns {
            src.push_str(&format!(
                "\tif which == \"{n}\" {{\n\t\tmatch {n}() {{\n\t\t\tOk(_) => return,\n\t\t\tErr(e) => {{\n\t\t\t\teprintln(\"error: {{e}}\")\n\t\t\t\tstd::os::exit(1)\n\t\t\t}},\n\t\t}}\n\t}}\n",
                n = t.name
            ));
        } else {
            src.push_str(&format!(
                "\tif which == \"{n}\" {{\n\t\t{n}()\n\t\treturn\n\t}}\n",
                n = t.name
            ));
        }
    }
    src.push_str("\teprintln(\"no such test: {which}\")\n\tstd::os::exit(2)\n}\n");
    let p = crate::parse_generated(src, "<test harness>");
    errors.extend(p.errors);
    file.items.extend(p.file.items);
    (tests, errors)
}

/// Test functions, and modules holding them, are made public so the
/// generated `main` can call them.
fn collect(
    items: &mut [Item],
    prefix: &str,
    tests: &mut Vec<TestCase>,
    calls: &mut Vec<bool>,
    errors: &mut Vec<Diagnostic>,
) {
    for it in items {
        let attrs = it.attrs.clone();
        match &mut it.kind {
            ItemKind::Mod {
                name,
                items: Some(inner),
                ..
            } => {
                let before = tests.len();
                let p = format!("{prefix}{}::", name.name);
                collect(inner, &p, tests, calls, errors);
                if tests.len() > before {
                    it.vis = Visibility::Public;
                }
            }
            ItemKind::Fn(f) if attrs.iter().any(|a| a.name.name == "test") => {
                if !f.params.is_empty() || f.self_param.is_some() || !f.generics.params.is_empty() {
                    errors.push(Diagnostic::error(
                        "a `#[test]` function must not take parameters or generic arguments",
                        f.name.span,
                    ));
                    continue;
                }
                let should_panic = attrs
                    .iter()
                    .find(|a| a.name.name == "should_panic")
                    .map(|a| a.args.as_deref().and_then(expected));
                tests.push(TestCase {
                    name: format!("{prefix}{}", f.name.name),
                    ignore: attrs.iter().any(|a| a.name.name == "ignore"),
                    should_panic,
                });
                calls.push(f.ret.is_some());
                it.vis = Visibility::Public;
            }
            _ => {}
        }
    }
}

/// The string in `expected = "..."`.
fn expected(args: &str) -> Option<String> {
    let rest = args.trim().strip_prefix("expected")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}
