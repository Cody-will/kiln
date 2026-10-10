//! End-to-end tests for the `kiln` binary.

use std::process::Command;

fn kiln(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .expect("run kiln");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn lex_prints_tokens_with_positions() {
    let (code, out, _) = kiln(&["lex", "examples/hello.kn"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("1:1\tKeyword(Fn)\n1:4\tIdent(\"main\")"));
    assert!(out.contains("Semi (newline)"));
}

#[test]
fn parse_prints_ast() {
    let (code, out, _) = kiln(&["parse", "examples/server.kn"]);
    assert_eq!(code, 0);
    assert!(out.contains("ShortDecl"));
    assert!(out.contains("Go("));
}

#[test]
fn errors_are_rendered_and_exit_nonzero() {
    let dir = std::env::temp_dir().join("kiln_cli_test");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bad.kn");
    std::fs::write(&file, "fn main() {\n\tx := 1 +\n}\n").unwrap();
    let (code, _, err) = kiln(&["parse", file.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(
        err.contains("error: expected expression, found `}`"),
        "{err}"
    );
    assert!(err.contains("bad.kn:3:1"), "{err}");
}

#[test]
fn usage_errors() {
    assert_eq!(kiln(&[]).0, 2);
    assert_eq!(kiln(&["parse", "does/not/exist.kn"]).0, 2);
    assert_eq!(kiln(&["--version"]).0, 0);
}

#[test]
fn check_accepts_examples() {
    for f in ["hello", "shapes", "server", "workers"] {
        let path = format!("examples/{f}.kn");
        let (code, out, err) = kiln(&["check", &path]);
        assert_eq!(code, 0, "{path}: {err}");
        assert!(out.contains("ok"));
    }
}

#[test]
fn check_reports_type_errors_with_caret() {
    let dir = std::env::temp_dir().join("kiln_cli_check_test");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bad.kn");
    std::fs::write(&file, "fn f() -> i32 {\n\treturn true\n}\n\nfn main() {}\n").unwrap();
    let (code, _, err) = kiln(&["check", file.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(
        err.contains("mismatched types: expected `i32`, found `bool`"),
        "{err}"
    );
    assert!(err.contains("return true"), "{err}");
    assert!(err.contains("^^^^"), "{err}");
}
