//! Differential test of the self-hosted parser (`compiler/`, written in
//! Kiln) against `kiln_parser`: builds `kilnc` with this `kiln`, runs
//! `kilnc --ast` on every `.kn` file in the repository (including the
//! corpora in `tests/parser/` and `tests/lexer/`), and requires its dump to
//! equal [`kiln_parser::dump::dump`] byte for byte: the whole AST with
//! spans, then every diagnostic in order.

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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let compiler = root.join("compiler");
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
    let kilnc = compiler.join("target/debug/kilnc");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    assert!(files.len() > 100, "found only {} .kn files", files.len());
    assert!(files.iter().any(|f| f.ends_with("tests/parser/errors.kn")));
    let mut failures = Vec::new();
    for chunk in files.chunks(32) {
        let out = Command::new(&kilnc)
            .arg("--ast")
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
            let want = kiln_parser::dump::dump(&src);
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
    eprintln!("{} files parsed identically", files.len());
}
