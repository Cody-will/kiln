//! Formats every Kiln file in the repository (examples, tests, the standard
//! library, and the code samples in the website and docs) and checks that
//! formatting succeeds, is idempotent, and leaves already-formatted files
//! alone.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn kn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            kn_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "kn") {
            out.push(p);
        }
    }
}

/// ```kiln code blocks in Markdown files.
fn md_samples(dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            md_samples(&p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
            let mut rest = text.as_str();
            let mut n = 0;
            while let Some(i) = rest.find("```kiln\n") {
                rest = &rest[i + 8..];
                let Some(end) = rest.find("```") else { break };
                n += 1;
                out.push((format!("{}#{n}", p.display()), rest[..end].to_string()));
                rest = &rest[end..];
            }
        }
    }
}

fn check(name: &str, src: &str, failures: &mut Vec<String>, must_be_formatted: bool) {
    match kiln_fmt::format(src) {
        Ok(once) => match kiln_fmt::format(&once) {
            Ok(twice) if twice == once => {
                if must_be_formatted && once != src {
                    failures.push(format!("{name}: not formatted (run `kiln fmt`)"));
                }
            }
            Ok(_) => failures.push(format!("{name}: not idempotent")),
            Err(e) => failures.push(format!("{name}: formatted output fails: {e}")),
        },
        // Samples that are fragments or deliberately wrong are skipped.
        Err(kiln_fmt::Error::Syntax(_)) => {}
        Err(e) => failures.push(format!("{name}: {e}")),
    }
}

#[test]
fn every_kiln_file_formats_idempotently() {
    let root = root();
    let mut files = Vec::new();
    for d in [
        "examples",
        "tests",
        "crates/kiln_resolve/std",
        "crates/kiln_resolve/src",
    ] {
        kn_files(&root.join(d), &mut files);
    }
    assert!(files.len() > 50, "found only {} files", files.len());
    let mut failures = Vec::new();
    // The parser corpus keeps unusual spellings on purpose (`[1, 2,]`,
    // `pub(pkg)`), which formatting would rewrite.
    files.retain(|f| !f.starts_with(root.join("tests/parser")));
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap().replace("\r\n", "\n");
        // Files under tests/ that are meant to fail to parse are skipped.
        check(&f.display().to_string(), &src, &mut failures, true);
    }
    let mut samples = Vec::new();
    for d in ["website/src", "docs", "README.md"] {
        md_samples(&root.join(d), &mut samples);
    }
    md_samples(&root, &mut Vec::new());
    for (name, src) in &samples {
        check(name, src, &mut failures, false);
    }
    assert!(
        failures.is_empty(),
        "{} problem(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
