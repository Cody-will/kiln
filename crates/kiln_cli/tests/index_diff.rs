//! Differential test of the language server's navigation index: `kilnc
//! --index` (`compiler/src/index.kn`) must print the same facts (each after
//! `fact\t`) as `kiln index --dump` ([`kiln_lsp::facts::rust`]) for every
//! `.kn` file in the repository, including ones with syntax and name errors.

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

#[test]
fn kilnc_index_matches_rust_index() {
    let root = root();
    let kilnc = build_kilnc();
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .kn files", files.len());
    let failures = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|s| {
        for t in 0..threads {
            let (files, kilnc, root, failures) = (&files, &kilnc, &root, &failures);
            s.spawn(move || {
                for f in files.iter().skip(t).step_by(threads) {
                    let want = Command::new(env!("CARGO_BIN_EXE_kiln"))
                        .args(["index", "--dump"])
                        .arg(f)
                        .output()
                        .expect("run kiln");
                    let got = Command::new(kilnc)
                        .arg("--index")
                        .arg(f)
                        .current_dir(root)
                        .env("KILNC_RESOLVE_DIR", root.join("crates/kiln_resolve"))
                        .output()
                        .expect("run kilnc");
                    let w = String::from_utf8_lossy(&want.stdout).to_string();
                    let g: String = String::from_utf8_lossy(&got.stdout)
                        .lines()
                        .filter_map(|l| l.strip_prefix("fact\t"))
                        .flat_map(|l| [l, "\n"])
                        .collect();
                    if !want.status.success() {
                        failures.lock().unwrap().push(format!(
                            "{}: kiln index --dump failed: {}",
                            f.display(),
                            String::from_utf8_lossy(&want.stderr)
                        ));
                    } else if w != g {
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
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} files index differently:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
