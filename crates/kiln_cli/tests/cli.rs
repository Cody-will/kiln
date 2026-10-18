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
    assert!(out.starts_with("1:1\tKeyword(Fn)\n1:5\tIdent(\"main\")"));
    assert!(out.contains("Semi (newline)"));
}

#[test]
fn parse_prints_ast() {
    let (code, out, _) = kiln(&["parse", "examples/server.kn"]);
    assert_eq!(code, 0);
    assert!(out.contains("ShortDecl"));
    assert!(out.contains("Spawn("));
}

#[test]
fn errors_are_rendered_and_exit_nonzero() {
    let dir = std::env::temp_dir().join("kiln_cli_test");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bad.kn");
    std::fs::write(&file, "fun main() {\n\tx := 1 +\n}\n").unwrap();
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
    std::fs::write(&file, "fun f() i32 {\n\treturn true\n}\n\nfun main() {}\n").unwrap();
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
        "fun take(s string) {}\n\nfun main() {\n\ts := string::from(\"hi\")\n\ttake(s)\n\ttake(s)\n}\n",
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

#[test]
fn version_and_installed_lib_dir() {
    let (code, out, _) = kiln(&["--version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("kiln "), "{out}");

    // Install std + runtime into a prefix, then build a fresh package
    // outside the repository against that installed library.
    let root = std::env::temp_dir().join(format!("kiln-install-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let lib = root.join("lib/kiln");
    let (code, _, err) = kiln(&["__install-lib", lib.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(lib.join("std/prelude.kn").is_file());
    assert!(lib.join("std/encoding/json.kn").is_file());
    let run = |args: &[&str], dir: &std::path::Path| {
        let o = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(args)
            .env("KILN_LIB_DIR", &lib)
            .current_dir(dir)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&o.stdout).into_owned();
        (
            o.status.code().unwrap_or(-1),
            s,
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    let (code, out, _) = run(&["version", "-v"], &root);
    assert_eq!(code, 0);
    assert!(out.contains(lib.to_str().unwrap()), "{out}");
    let (code, _, err) = run(&["new", "app"], &root);
    assert_eq!(code, 0, "{err}");
    for f in ["kiln.toml", "src/main.kn", ".gitignore", "README.md"] {
        assert!(root.join("app").join(f).is_file(), "missing {f}");
    }
    let (code, out, err) = run(&["run"], &root.join("app/src"));
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "Hello, world!\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compiler_selection_and_fallback() {
    let dir = std::env::temp_dir().join(format!("kiln_cli_compiler_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("hello");
    let out_s = out.to_str().unwrap();
    // The built-in compiler, explicitly.
    let (code, _, err) = kiln(&["build", "--compiler=rust", "examples/hello.kn", "-o", out_s]);
    assert_eq!(code, 0, "{err}");
    // `--compiler=kilnc` without kilnc installed warns and falls back.
    let o = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            "--compiler=kilnc",
            "examples/hello.kn",
            "-o",
            out_s,
        ])
        .env("KILNC", dir.join("no-such-kilnc"))
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("kilnc is not installed"));
    // A kilnc that fails: kiln rebuilds with the built-in compiler.
    let fake = dir.join("fake-kilnc");
    std::fs::write(&fake, "#!/bin/sh\necho broken >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let o = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", "examples/hello.kn", "-o", out_s])
            .env("KILNC", &fake)
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .output()
            .unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("note: kilnc failed"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
