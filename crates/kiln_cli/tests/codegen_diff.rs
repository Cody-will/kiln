//! Output-comparison test of the self-hosted code generator
//! (`compiler/src/codegen.kn`): every `tests/run/*.kn` program is compiled
//! by `kilnc --build` (LLVM IR text, compiled and linked by `clang`), run,
//! and its output (stdout, stderr, then `[exit N]`) must match the `.out`
//! file that `kiln build` is held to by `tests/run.rs`. Skipped, with a
//! note, when `clang` is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn have_clang() -> bool {
    Command::new(std::env::var("KILN_CLANG").unwrap_or_else(|_| "clang".into()))
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Builds `compiler/` and returns `kilnc`.
fn build_kilnc() -> PathBuf {
    let compiler = root().join("compiler");
    let st = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("build")
        .current_dir(&compiler)
        .env("KILN_RUN_FLAGS", "")
        .output()
        .expect("run kiln build");
    assert!(
        st.status.success(),
        "building compiler/ failed:\n{}",
        String::from_utf8_lossy(&st.stderr)
    );
    compiler.join("target/debug/kilnc")
}

#[test]
fn kilnc_programs_match_expected_output() {
    if !have_clang() {
        eprintln!("note: clang not found; skipping the self-hosted codegen comparison");
        return;
    }
    let kilnc = build_kilnc();
    let dir = std::env::temp_dir().join(format!("kiln-cgdiff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rt = dir.join(kiln_runtime::LIB_NAME);
    std::fs::write(&rt, kiln_runtime::LIB).unwrap();
    std::fs::write(dir.join(kiln_runtime::TLS_LIB_NAME), kiln_runtime::TLS_LIB).unwrap();
    let run = root().join("tests/run");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&run)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "kn"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut failures = Vec::new();
    for src in &files {
        let stem = src.file_stem().unwrap().to_str().unwrap();
        let exe = dir.join(stem);
        let text = std::fs::read_to_string(src).unwrap();
        let flags: Vec<&str> = text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// flags:"))
            .map(|f| f.split_whitespace().collect())
            .unwrap_or_default();
        let b = Command::new(&kilnc)
            .arg("--build")
            .args(&flags)
            .arg(src)
            .arg("-o")
            .arg(&exe)
            .env("KILN_RT", &rt)
            .env("KILNC_RESOLVE_DIR", root().join("crates/kiln_resolve"))
            .output()
            .unwrap();
        if !b.status.success() {
            failures.push(format!(
                "{stem}: kilnc --build failed:\n{}",
                String::from_utf8_lossy(&b.stderr)
            ));
            continue;
        }
        let r = Command::new(&exe).current_dir(&dir).output().unwrap();
        let mut out = String::from_utf8_lossy(&r.stdout).into_owned();
        out.push_str(&String::from_utf8_lossy(&r.stderr));
        match r.status.code() {
            Some(0) => {}
            Some(c) => out.push_str(&format!("[exit {c}]\n")),
            None => out.push_str(&format!("[terminated: {}]\n", r.status)),
        }
        let want = std::fs::read_to_string(src.with_extension("out")).unwrap();
        if out != want {
            failures.push(format!("{stem}: output differs:\n--- got\n{out}--- want\n{want}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {} programs differ:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
