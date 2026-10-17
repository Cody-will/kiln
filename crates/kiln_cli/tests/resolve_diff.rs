//! Differential test of the self-hosted resolver (`compiler/src/resolve.kn`)
//! against `kiln_resolve`: for every `.kn` file in the repository
//! (including the error corpus `tests/resolve/errors.kn`), `kilnc --resolve`
//! must print the same table as `kiln resolve --dump`
//! ([`kiln_resolve::dump::dump`]): every definition, local, path
//! resolution, binding, impl, function, static and constant function with
//! the same ids, then every error in order. Both sides parse the prelude
//! and standard library (from `crates/kiln_resolve`), so the ids also check
//! them. Derive spans depend on what a process parsed before, so each file
//! gets fresh processes.

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
pub fn build_kilnc() -> PathBuf {
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
fn self_hosted_resolver_matches_rust_resolver() {
    let root = root();
    let kilnc = build_kilnc();
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .kn files", files.len());
    assert!(files.iter().any(|f| f.ends_with("tests/resolve/errors.kn")));
    let failures = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|s| {
        for t in 0..threads {
            let (files, kilnc, root, failures) = (&files, &kilnc, &root, &failures);
            s.spawn(move || {
                for f in files.iter().skip(t).step_by(threads) {
                    let want = Command::new(env!("CARGO_BIN_EXE_kiln"))
                        .args(["resolve", "--dump"])
                        .arg(f)
                        .output()
                        .expect("run kiln");
                    let got = Command::new(kilnc)
                        .arg("--resolve")
                        .arg(f)
                        .current_dir(root)
                        .output()
                        .expect("run kilnc");
                    if !got.status.success() {
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
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} files resolve differently:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}
