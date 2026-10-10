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

#[test]
fn check_reports_borrow_errors_with_notes() {
    let dir = std::env::temp_dir().join("kiln_cli_borrowck_test");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("moved.kn");
    std::fs::write(
        &file,
        "fn take(s: String) {}\n\nfn main() {\n\ts := String::from(\"hi\")\n\ttake(s)\n\ttake(s)\n}\n",
    )
    .unwrap();
    let (code, _, err) = kiln(&["check", file.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("error: use of moved value: `s`"), "{err}");
    assert!(err.contains("6 | \ttake(s)"), "{err}");
    assert!(err.contains("note: value moved here"), "{err}");
    assert!(err.contains("5 | \ttake(s)"), "{err}");
    assert!(err.contains("help:"), "{err}");
}

#[test]
fn mir_dumps_bodies() {
    let (code, out, err) = kiln(&["mir", "examples/hello.kn"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("main Fn"), "{out}");
    assert!(out.contains("bb0:"), "{out}");
    assert!(out.contains("return"), "{out}");
}

#[test]
fn test_command_runs_passing_tests() {
    let (code, out, err) = kiln(&["test", "tests/kiln_test/passing.kn"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("running 5 tests"), "{out}");
    assert!(out.contains("test adds ... ok"));
    assert!(out.contains("test returns_ok ... ok"));
    assert!(out.contains("test divides_by_zero ... ok"));
    assert!(out.contains("test slow ... ignored"));
    assert!(out.contains("test inner::nested ... ok"));
    assert!(
        out.contains("test result: ok. 4 passed; 0 failed; 1 ignored"),
        "{out}"
    );

    let (code, out, _) = kiln(&["test", "--run", "nest", "tests/kiln_test/passing.kn"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("1 passed; 0 failed; 0 ignored; 4 filtered out"),
        "{out}"
    );
}

#[test]
fn test_command_reports_failures() {
    let (code, out, _) = kiln(&["test", "tests/kiln_test/failing.kn"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("test passes ... ok"));
    assert!(out.contains("test wrong_sum ... FAILED"));
    assert!(out.contains("captured output"));
    assert!(out.contains("left: 4\n right: 5"), "{out}");
    assert!(out.contains("test returns_err ... FAILED"));
    assert!(out.contains("error: invalid integer literal"), "{out}");
    assert!(out.contains("test did not panic as expected"));
    assert!(
        out.contains("test result: FAILED. 1 passed; 3 failed"),
        "{out}"
    );
}

#[test]
fn test_command_discovers_files_in_directories() {
    let (code, out, _) = kiln(&["test", "--run", "adds", "tests/kiln_test"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("passing.kn"), "{out}");
    assert!(
        out.contains("test result: ok. 1 passed; 0 failed; 0 ignored; 8 filtered out"),
        "{out}"
    );
}
