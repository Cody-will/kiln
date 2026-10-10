//! End-to-end tests: compile every `tests/run/*.kn` program with `kiln build`,
//! run it, and compare its output with the matching `.out` file.
//!
//! The expected file holds the program's stdout, then its stderr, then
//! `[exit N]` when the exit status is not 0. A first line `// flags: ...` passes
//! extra `kiln build` flags (e.g. `--release`). Set `KILN_BLESS=1` to rewrite the
//! expected files from the actual output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn kiln() -> &'static str {
    env!("CARGO_BIN_EXE_kiln")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kiln-e2e-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds and runs `src`, returning the output in the `.out` format.
fn build_and_run(src: &Path, dir: &Path) -> Result<String, String> {
    let stem = src.file_stem().unwrap().to_str().unwrap();
    let exe = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
    // A first line `// flags: --release` passes build flags.
    let text = std::fs::read_to_string(src).unwrap();
    let flags: Vec<&str> = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("// flags:"))
        .map(|f| f.split_whitespace().collect())
        .unwrap_or_default();
    let b = Command::new(kiln())
        .arg("build")
        .args(&flags)
        .arg(src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    if !b.status.success() {
        return Err(format!(
            "`kiln build {}` failed:\n{}{}",
            src.display(),
            String::from_utf8_lossy(&b.stdout),
            String::from_utf8_lossy(&b.stderr)
        ));
    }
    let r = Command::new(&exe)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let mut out = String::from_utf8_lossy(&r.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&r.stderr));
    match r.status.code() {
        Some(0) => {}
        Some(c) => out.push_str(&format!("[exit {c}]\n")),
        None => out.push_str(&format!("[terminated: {}]\n", r.status)),
    }
    Ok(out.replace("\r\n", "\n"))
}

#[test]
fn run_programs() {
    let dir = root().join("tests/run");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "kn"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no programs in {}", dir.display());
    let tmp = scratch("run");
    let bless = std::env::var_os("KILN_BLESS").is_some();
    let mut failures = Vec::new();
    for f in &files {
        let expected_path = f.with_extension("out");
        match build_and_run(f, &tmp) {
            Err(e) => failures.push(e),
            Ok(actual) => {
                if bless {
                    std::fs::write(&expected_path, &actual).unwrap();
                    continue;
                }
                let expected = std::fs::read_to_string(&expected_path)
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                if actual.trim_end() != expected.trim_end() {
                    failures.push(format!(
                        "{}: output differs\n--- expected\n{expected}\n--- actual\n{actual}",
                        f.display()
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "{} of {} programs failed:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

#[test]
fn hello_example_runs() {
    let tmp = scratch("hello");
    let out = build_and_run(&root().join("examples/hello.kn"), &tmp).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(out, "Hello, Kiln!\n");
}

#[test]
fn kiln_run_forwards_exit_status() {
    let out = Command::new(kiln())
        .arg("run")
        .arg(root().join("tests/run/main_result.kn"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Error: 50 is too big"));
}

#[test]
fn unsupported_features_are_reported_not_miscompiled() {
    let dir = scratch("unsupported");
    let src = dir.join("slice.kn");
    std::fs::write(
        &src,
        "fn main() {\n\tv := [1, 2, 3]\n\tmatch &v[..] {\n\t\t[a, ..] => println(\"{a}\"),\n\t\t_ => {},\n\t}\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln())
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("slice"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("code generation does not support slice patterns"),
        "{err}"
    );
    assert!(err.contains("slice.kn:4:"), "{err}");
}

#[test]
fn borrow_errors_stop_the_build() {
    let dir = scratch("borrow");
    let src = dir.join("moved.kn");
    std::fs::write(
        &src,
        "fn main() {\n\ts := String::from(\"x\")\n\tt := s\n\tprintln(s)\n\tprintln(t)\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln())
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("moved"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("moved"));
}
