//! Differential test of the self-hosted MIR lowering
//! (`compiler/src/lower.kn`) against `kiln_mir`: `kilnc --mir` must print
//! the same MIR as `kiln mir --dump` ([`kiln_mir::dump::dump`]) for every
//! `.kn` file in the repository and for every Kiln snippet in the MIR and
//! borrow checker unit tests (`crates/kiln_mir/src/tests.rs`,
//! `crates/kiln_borrowck/src/tests.rs`, and the type checker's, written to
//! a temporary directory):
//! every body of every user function with its locals, statements,
//! terminators, spans, captures and lowering errors. Each file gets fresh
//! processes (derive spans depend on what a process parsed before).

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

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
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

/// The Kiln programs in a Rust test file: every string literal passed
/// directly to a call (`ok("...")`, `err1(\n "...")`) that contains `fun `.
pub fn snippets(rust: &str) -> Vec<String> {
    let b = rust.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'(' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= b.len() || b[j] != b'"' {
            i += 1;
            continue;
        }
        j += 1;
        let mut s = String::new();
        while j < b.len() && b[j] != b'"' {
            if b[j] == b'\\' {
                match b[j + 1] {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'0' => s.push('\0'),
                    b'\n' => {
                        j += 2;
                        while j < b.len() && b[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        continue;
                    }
                    c => s.push(c as char),
                }
                j += 2;
            } else {
                let ch = rust[j..].chars().next().unwrap();
                s.push(ch);
                j += ch.len_utf8();
            }
        }
        if s.contains("fun ") {
            out.push(s);
        }
        i = j + 1;
    }
    out
}

#[test]
fn self_hosted_mir_lowering_matches_rust_lowering() {
    let root = root();
    let kilnc = build_kilnc();
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .kn files", files.len());
    let tmp = std::env::temp_dir().join(format!("kiln-mir-diff-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut snips = Vec::new();
    for t in [
        "crates/kiln_mir/src/tests.rs",
        "crates/kiln_borrowck/src/tests.rs",
        "crates/kiln_typeck/src/tests.rs",
    ] {
        snips.extend(snippets(&std::fs::read_to_string(root.join(t)).unwrap()));
    }
    assert!(snips.len() > 100, "found only {} snippets", snips.len());
    for (k, s) in snips.iter().enumerate() {
        let p = tmp.join(format!("case{k:03}.kn"));
        std::fs::write(&p, s).unwrap();
        files.push(p);
    }
    let failures = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|s| {
        for t in 0..threads {
            let (files, kilnc, root, failures) = (&files, &kilnc, &root, &failures);
            s.spawn(move || {
                for f in files.iter().skip(t).step_by(threads) {
                    let want = Command::new(env!("CARGO_BIN_EXE_kiln"))
                        .args(["mir", "--dump"])
                        .arg(f)
                        .output()
                        .expect("run kiln");
                    let got = Command::new(kilnc)
                        .arg("--mir")
                        .arg(f)
                        .current_dir(root)
                        .output()
                        .expect("run kilnc");
                    if !want.status.success() {
                        failures.lock().unwrap().push(format!(
                            "{}: kiln mir --dump failed: {}",
                            f.display(),
                            String::from_utf8_lossy(&want.stderr)
                        ));
                    } else if !got.status.success() {
                        failures.lock().unwrap().push(format!(
                            "{}: kilnc failed: {}",
                            f.display(),
                            String::from_utf8_lossy(&got.stderr)
                        ));
                    } else if want.stdout != got.stdout {
                        let (w, g) = (
                            String::from_utf8_lossy(&want.stdout),
                            String::from_utf8_lossy(&got.stdout),
                        );
                        let line = w
                            .lines()
                            .zip(g.lines())
                            .find(|(a, b)| a != b)
                            .map(|(a, b)| format!("want `{a}`, got `{b}`"))
                            .unwrap_or_else(|| "lengths differ".into());
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{}: {line}", f.display()));
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&tmp);
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} files lower differently:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
