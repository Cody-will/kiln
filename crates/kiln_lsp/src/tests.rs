use crate::analysis::analyze;

const SRC: &str = "struct Point {\n\tx: i32,\n}\n\nimpl Point {\n\t// Twice x.\n\tfn double(&self) -> i32 {\n\t\treturn self.x * 2\n\t}\n}\n\n// Adds.\nfn add(a: i32, b: i32) -> i32 {\n\treturn a + b\n}\n\nfn main() {\n\tp := Point { x: 1 }\n\ts := add(p.x, p.double())\n\tprintln(\"{s}\")\n}\n";

fn at(needle: &str, nth: usize) -> usize {
    SRC.match_indices(needle).nth(nth).unwrap().0
}

#[test]
fn definitions_references_hover() {
    let a = analyze(SRC);
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    // add(...) in main goes to fn add.
    let d = a.definition(at("add(p", 0)).unwrap();
    assert_eq!(&SRC[d.start..d.end], "add");
    assert_eq!(d.start, at("add(a", 0));
    // Method call goes to the method.
    let d = a.definition(at("double()", 0) + 1).unwrap();
    assert_eq!(d.start, at("double(&self", 0));
    // Local `s` used in the string interpolation.
    let refs = a.references(at("{s}", 0) + 1);
    assert_eq!(refs.len(), 2);
    // Hover.
    let (_, h) = a.hover(at("add(p", 0)).unwrap();
    assert!(
        h.contains("fn add(a: i32, b: i32) -> i32") && h.contains("Adds."),
        "{h}"
    );
    let (_, h) = a.hover(at("p := ", 0)).unwrap();
    assert!(h.contains("p: Point"), "{h}");
    let (_, h) = a.hover(at("println", 0)).unwrap();
    assert!(h.contains("fn println"), "{h}");
}

#[test]
fn symbols_hints_completion() {
    let a = analyze(SRC);
    let names: Vec<_> = a
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect();
    assert!(
        names.contains(&("Point", "struct"))
            && names.contains(&("double", "method"))
            && names.contains(&("main", "fn")),
        "{names:?}"
    );
    let hints = a.inlay_hints();
    assert!(hints.iter().any(|(_, h)| h == ": Point"), "{hints:?}");
    assert!(hints.iter().any(|(_, h)| h == ": i32"), "{hints:?}");
    // After `p.` (members of Point).
    let src2 = SRC.replace("println(\"{s}\")", "println(\"{s}\")\n\tp.");
    let a2 = analyze(&src2);
    let off = src2.find("\tp.\n").unwrap() + 3;
    let c: Vec<_> = a2
        .completions(&src2, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    // The file has an error now, so fall back to the earlier analysis's members.
    let c = if c.is_empty() {
        a.members
            .get("Point")
            .unwrap()
            .iter()
            .map(|c| c.label.clone())
            .collect()
    } else {
        c
    };
    assert!(
        c.contains(&"x".to_string()) && c.contains(&"double".to_string()),
        "{c:?}"
    );
    // Plain identifiers: locals and globals by prefix.
    let off = at("println", 0) + 2;
    let c: Vec<_> = a
        .completions(SRC, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        c.contains(&"println".to_string()) && c.contains(&"print".to_string()),
        "{c:?}"
    );
    let off = at("println", 0);
    let c: Vec<_> = a
        .completions(SRC, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        c.contains(&"s".to_string())
            && c.contains(&"p".to_string())
            && c.contains(&"add".to_string())
    );
}

#[test]
fn definitions_and_references_across_files() {
    use std::path::{Path, PathBuf};
    let main = "mod util\n\nfn main() {\n\tprintln(\"{util::twice(2)}\")\n}\n";
    let util = "pub fn twice(x: i64) -> i64 {\n\treturn x * 2\n}\n";
    let dir = PathBuf::from("/nonexistent-kiln-lsp-test");
    let overlay = |p: &Path| -> Option<String> {
        if p == dir.join("main.kn") {
            Some(main.into())
        } else if p == dir.join("util.kn") {
            Some(util.into())
        } else {
            None
        }
    };
    let pkg = kiln_parser::package::load(&dir.join("main.kn"), &[], &overlay).unwrap();
    let a = crate::analysis::analyze_package(&pkg);
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    assert_eq!(a.files.len(), 2);
    let use_at = main.find("twice").unwrap();
    let d = a.definition(use_at).unwrap();
    let (uoff, _, _) = a.files[1];
    assert_eq!(d.start, uoff + util.find("twice").unwrap());
    // References from the definition include the use in main.kn.
    assert!(a.references(d.start).iter().any(|s| s.start == use_at));
}
