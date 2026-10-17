//! Differential test of the self-hosted parser (`compiler/`, written in
//! Kiln) against `kiln_parser`: builds `kilnc` with this `kiln`, runs
//! `kilnc --ast` on every `.kn` file in the repository (including the
//! corpora in `tests/parser/` and `tests/lexer/`), and requires its dump to
//! equal [`kiln_parser::dump::dump`] byte for byte: the whole AST with
//! spans, then every diagnostic in order. A second test does the same for
//! derive expansion (`kilnc --derive`, `tests/parser/derive.kn`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if p.is_dir() {
            collect(&p, out);
        } else if name.ends_with(".kn") {
            out.push(p);
        }
    }
}

#[test]
fn self_hosted_parser_matches_rust_parser() {
    diff_all("--ast", kiln_parser::dump::dump, "parsed");
}

/// The same for derive expansion: `kilnc --derive` against
/// [`kiln_parser::dump::dump_derive`] (the generated source text).
#[test]
fn self_hosted_derive_matches_rust_derive() {
    diff_all("--derive", kiln_parser::dump::dump_derive, "derived");
}

/// Builds `compiler/` once (the tests run in parallel) and returns `kilnc`.
fn kilnc() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
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
        })
        .clone()
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Derive expansion end to end: the AST after expansion, for each file
/// that derives `Json`. Generated spans depend on what was parsed before
/// in the process, so each file gets fresh `kiln` and `kilnc` processes.
#[test]
fn self_hosted_expansion_matches_rust_expansion() {
    let kilnc = kilnc();
    let mut files = Vec::new();
    collect(&root(), &mut files);
    files.sort();
    let mut n = 0;
    for f in &files {
        let src = std::fs::read_to_string(f).expect("read");
        if !src.contains("Json") {
            continue;
        }
        n += 1;
        let want = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["parse", "--dump-expanded"])
            .arg(f)
            .output()
            .expect("run kiln");
        let got = Command::new(&kilnc)
            .arg("--ast-expanded")
            .arg(f)
            .output()
            .expect("run kilnc");
        assert!(got.status.success(), "kilnc failed on {}", f.display());
        assert!(
            want.stdout == got.stdout,
            "{}: expanded ASTs differ",
            f.display()
        );
    }
    assert!(n >= 3, "only {n} files derive Json");
}

fn diff_all(flag: &str, dump: fn(&str) -> String, what: &str) {
    let root = root();
    let kilnc = kilnc();
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .kn files", files.len());
    assert!(files.iter().any(|f| f.ends_with("tests/parser/errors.kn")));
    let mut failures = Vec::new();
    for chunk in files.chunks(32) {
        let out = Command::new(&kilnc)
            .arg(flag)
            .args(chunk)
            .output()
            .expect("run kilnc");
        assert!(
            out.status.success(),
            "kilnc failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).expect("utf-8 dump");
        // `== <path>` headers split the dumps (only with several files).
        let mut dumps: Vec<String> = Vec::new();
        for line in text.split_inclusive('\n') {
            if line.starts_with("== ") && chunk.len() > 1 {
                dumps.push(String::new());
            } else if let Some(d) = dumps.last_mut() {
                d.push_str(line);
            } else {
                dumps.push(line.to_string());
            }
        }
        assert_eq!(dumps.len(), chunk.len(), "one dump per file");
        for (f, got) in chunk.iter().zip(&dumps) {
            let src = std::fs::read_to_string(f).expect("read");
            let want = dump(&src);
            if *got != want {
                let line = want
                    .lines()
                    .zip(got.lines())
                    .position(|(a, b)| a != b)
                    .unwrap_or(want.lines().count().min(got.lines().count()));
                failures.push(format!(
                    "{}: first difference at dump line {}:\n  rust: {:?}\n  kiln: {:?}",
                    f.display(),
                    line + 1,
                    want.lines().nth(line),
                    got.lines().nth(line)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} file(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{} files {what} identically", files.len());
}
