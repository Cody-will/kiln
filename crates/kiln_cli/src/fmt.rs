//! `kiln fmt [--check] [--stdin] [paths...]`: formats Kiln source files in
//! place (see `kiln_fmt`).
//!
//! * With no paths, formats every `.kn` file under the current directory.
//! * `--check` changes nothing: it lists files that need formatting and exits
//!   with status 1 if there are any (for CI).
//! * `--stdin` formats standard input to standard output (for editors).
//!
//! Files with syntax errors are reported and left unchanged (status 1).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Runs `kiln fmt` with the arguments after `fmt`.
pub fn run(args: &[String]) -> ExitCode {
    let mut check = false;
    let mut stdin = false;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            "--stdin" => stdin = true,
            s if s.starts_with('-') => {
                eprintln!(
                    "error: unknown option `{s}`\nusage: kiln fmt [--check] [--stdin] [paths...]"
                );
                return ExitCode::from(2);
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if stdin {
        let mut src = String::new();
        if std::io::stdin().read_to_string(&mut src).is_err() {
            eprintln!("error: could not read standard input");
            return ExitCode::from(2);
        }
        return match kiln_fmt::format(&src) {
            Ok(out) => {
                let _ = std::io::stdout().write_all(out.as_bytes());
                ExitCode::SUCCESS
            }
            Err(e) => {
                report("<stdin>", &src, &e);
                ExitCode::from(1)
            }
        };
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    let mut files = Vec::new();
    for p in &paths {
        if p.is_dir() {
            collect(p, &mut files);
        } else {
            files.push(p.clone());
        }
    }
    let mut failed = false;
    let mut unformatted = 0;
    for f in &files {
        let name = f.display().to_string();
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read `{name}`: {e}");
                failed = true;
                continue;
            }
        };
        // Line endings are normalized to \n.
        let input = src.replace("\r\n", "\n");
        match kiln_fmt::format(&input) {
            Ok(out) if out == src => {}
            Ok(out) => {
                unformatted += 1;
                if check {
                    println!("{name}");
                } else if let Err(e) = std::fs::write(f, out) {
                    eprintln!("error: cannot write `{name}`: {e}");
                    failed = true;
                }
            }
            Err(e) => {
                report(&name, &input, &e);
                failed = true;
            }
        }
    }
    if check && unformatted > 0 {
        eprintln!("{unformatted} file(s) need formatting; run `kiln fmt`");
        return ExitCode::from(1);
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report(name: &str, src: &str, e: &kiln_fmt::Error) {
    match e {
        kiln_fmt::Error::Syntax(diags) => {
            eprintln!("{name}: not formatted: the file has syntax errors");
            crate::report(name, src, diags);
        }
        kiln_fmt::Error::Internal(m) => {
            eprintln!("{name}: not formatted: {m} (this is a bug in kiln fmt; please report it)")
        }
    }
}

/// `.kn` files under `dir`, skipping hidden directories and `target`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                collect(&p, out);
            }
        } else if p.extension().is_some_and(|e| e == "kn") {
            out.push(p);
        }
    }
}
