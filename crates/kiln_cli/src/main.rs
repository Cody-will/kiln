//! The `kiln` command-line driver.
//!
//! Currently supports inspecting the front end:
//!
//! - `kiln lex <file>`: print tokens with line:column positions.
//! - `kiln parse <file>`: print the AST.
//! - `kiln check <file>`: parse, resolve names, type-check, and borrow-check; print diagnostics.
//! - `kiln mir <file>`: print the lowered MIR (for compiler debugging).
//! - `kiln build [--release] <file> [-o <output>]`: compile to a native executable
//!   (debug builds panic on integer overflow; release builds wrap).
//! - `kiln run [--release] <file> [args...]`: build to a temporary executable and run it.
//! - `kiln test [--release] [--run <substr>] [--ignored] [--timeout <secs>] [paths...]`:
//!   build and run `#[test]` functions (see [`test`]).
//!
//! - `kiln fmt [--check] [--stdin] [paths...]`: format source files (see [`fmt`]).
//! - `kiln lsp`: the language server, speaking LSP on stdin/stdout.
//!
//! Exit status is 0 on success, 1 when the file has errors, and 2 on usage errors
//! (`kiln run` exits with the program's status).

mod fmt;
mod package;
mod test;

use kiln_lexer::{lex, Diagnostic, LineIndex, TokenKind};
use std::process::ExitCode;

const USAGE: &str = "\
kiln - the Kiln language toolchain

USAGE:
    kiln new <name> [--lib] Create a package (kiln.toml, src/main.kn or src/lib.kn)
    kiln check [file.kn]    Resolve names, type-check, and borrow-check
    kiln build [--release] [file.kn] [-o <output>]
                            Compile to a native executable (a package goes to
                            target/debug/<name> or target/release/<name>)
    kiln run [--release] [file.kn] [args...]
                            Build and run (in a package: kiln run [-- args...])
    kiln lex <file.kn>      Print the token stream
    kiln parse <file.kn>    Print the syntax tree
    kiln mir <file.kn>      Print the mid-level IR (compiler debugging)

    Without a file, check/build/run/test use the package containing the
    current directory (the nearest kiln.toml).
    kiln test [--release] [--run <substr>] [--ignored] [--timeout <secs>] [paths...]
                            Run @test functions in the given files, or in every
                            .kn file under the given directories (default: .)
    kiln fmt [--check] [--stdin] [paths...]
                            Format .kn files in place (default: every file under .);
                            --check lists unformatted files and exits 1
    kiln lsp                The language server (LSP over stdio), for editors

    Debug builds (the default) panic on integer overflow; --release builds wrap.
    kiln help               Show this message
    kiln --version          Show the version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, file]
            if (cmd == "lex" || cmd == "parse" || cmd == "mir")
                && !std::path::Path::new(file).is_dir() =>
        {
            run(cmd, file)
        }
        [cmd, rest @ ..] if cmd == "check" && rest.len() <= 1 => {
            check(rest.first().map(String::as_str))
        }
        [cmd, rest @ ..] if cmd == "new" => package::new(rest),
        [cmd, rest @ ..] if cmd == "build" => {
            let mut opts = kiln_codegen::Options::default();
            let (mut file, mut out) = (None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--release" => opts.release = true,
                    "-o" => out = it.next(),
                    _ if file.is_none() && !a.starts_with('-') => file = Some(a),
                    _ => return usage_error(),
                }
            }
            match rest.iter().any(|a| a == "-o") == out.is_some() {
                true => build(file.map(String::as_str), out, opts),
                false => usage_error(),
            }
        }
        [cmd, rest @ ..] if cmd == "run" => {
            // Options come before the file; everything after it goes to the program.
            let mut opts = kiln_codegen::Options::default();
            let mut i = 0;
            while i < rest.len() && rest[i] == "--release" {
                opts.release = true;
                i += 1;
            }
            // `kiln run [file.kn | dir] [args]`; in a package, `kiln run [-- args]`.
            match rest.get(i) {
                Some(f) if f == "--" => run_program(None, &rest[i + 1..], opts),
                Some(f)
                    if !f.starts_with('-')
                        && (f.ends_with(".kn") || std::path::Path::new(f).is_dir()) =>
                {
                    let args = match rest.get(i + 1) {
                        Some(d) if d == "--" => &rest[i + 2..],
                        _ => &rest[i + 1..],
                    };
                    run_program(Some(f), args, opts)
                }
                None => run_program(None, &[], opts),
                Some(f) if !f.starts_with('-') => run_program(Some(f), &rest[i + 1..], opts),
                _ => usage_error(),
            }
        }
        [cmd, rest @ ..] if cmd == "fmt" => fmt::run(rest),
        [cmd, rest @ ..] if cmd == "lsp" && rest.iter().all(|a| a == "--stdio") => {
            match kiln_lsp::server::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("kiln lsp: {e}");
                    ExitCode::from(1)
                }
            }
        }
        [cmd, rest @ ..] if cmd == "test" => match test::parse_args(rest) {
            Some(o) => test::run(o),
            None => usage_error(),
        },
        [flag] if flag == "--version" || flag == "-V" => {
            println!("kiln {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [cmd] if cmd == "help" || cmd == "--help" || cmd == "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(cmd: &str, file: &str) -> ExitCode {
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{file}`: {e}");
            return ExitCode::from(2);
        }
    };
    let errors = if cmd == "lex" {
        let lexed = lex(&src);
        let idx = LineIndex::new(&src);
        for t in &lexed.tokens {
            let (line, col) = idx.line_col(t.span.start);
            println!("{line}:{col}\t{}", describe(&t.kind));
        }
        lexed.errors
    } else if cmd == "mir" {
        mir(&src)
    } else {
        let parsed = kiln_parser::parse(&src);
        println!("{:#?}", parsed.file);
        parsed.errors
    };
    report(file, &src, &errors)
}

fn default_output(file: &str) -> std::path::PathBuf {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a");
    std::path::PathBuf::from(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn build_to(
    p: &package::Program,
    out: &std::path::Path,
    opts: kiln_codegen::Options,
) -> Result<(), ExitCode> {
    match kiln_codegen::build_parsed(p.pkg.parse.clone(), out, opts) {
        Ok(()) => Ok(()),
        Err(kiln_codegen::BuildError::Diagnostics(errs)) => Err(report(&p.name(), p.src(), &errs)),
        Err(kiln_codegen::BuildError::Other(msg)) => {
            eprintln!("error: {msg}");
            Err(ExitCode::from(1))
        }
    }
}

/// `kiln check [file.kn | dir]`.
fn check(target: Option<&str>) -> ExitCode {
    let p = match package::load(target, true) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let errors = kiln_borrowck::check_parse(&p.pkg.parse);
    if errors.is_empty() {
        println!("{}: ok", p.name());
    }
    report(&p.name(), p.src(), &errors)
}

/// Builds a package (skipping the build when its inputs are unchanged) or a
/// single file. Returns the executable's path.
fn build_program(
    p: &package::Program,
    out: Option<&String>,
    opts: kiln_codegen::Options,
) -> Result<(std::path::PathBuf, bool), ExitCode> {
    let out = match (out, &p.manifest) {
        (Some(o), _) => std::path::PathBuf::from(o),
        (None, Some(m)) if p.root.ends_with("src/main.kn") => package::output_path(m, opts.release),
        _ => default_output(&p.name()),
    };
    let stamp = out.with_extension("fingerprint");
    let fp = package::fingerprint(p, opts.release);
    let cached = p.manifest.is_some()
        && out.is_file()
        && std::fs::read_to_string(&stamp).is_ok_and(|s| s == fp);
    if cached {
        return Ok((out, true));
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: cannot create {}: {e}", dir.display());
            return Err(ExitCode::from(1));
        }
    }
    build_to(p, &out, opts)?;
    if p.manifest.is_some() {
        let _ = std::fs::write(&stamp, fp);
    }
    Ok((out, false))
}

fn build(target: Option<&str>, out: Option<&String>, opts: kiln_codegen::Options) -> ExitCode {
    let p = match package::load(target, false) {
        Ok(p) => p,
        Err(c) => return c,
    };
    match build_program(&p, out, opts) {
        Ok((out, cached)) => {
            println!(
                "{} {}",
                if cached { "up to date:" } else { "built" },
                out.display()
            );
            ExitCode::SUCCESS
        }
        Err(c) => c,
    }
}

fn run_program(target: Option<&str>, args: &[String], opts: kiln_codegen::Options) -> ExitCode {
    let p = match package::load(target, false) {
        Ok(p) => p,
        Err(c) => return c,
    };
    // Packages build into target/ (and are cached); single files into a
    // temporary directory.
    let tmp = std::env::temp_dir().join(format!("kiln-run-{}", std::process::id()));
    let exe = if p.manifest.is_some() && p.root.ends_with("src/main.kn") {
        match build_program(&p, None, opts) {
            Ok((e, _)) => e,
            Err(c) => return c,
        }
    } else {
        if let Err(e) = std::fs::create_dir_all(&tmp) {
            eprintln!("error: cannot create {}: {e}", tmp.display());
            return ExitCode::from(1);
        }
        let exe = tmp.join(default_output(&p.name()));
        if let Err(c) = build_to(&p, &exe, opts) {
            let _ = std::fs::remove_dir_all(&tmp);
            return c;
        }
        exe
    };
    let exe = if exe.is_relative() && exe.parent().is_some_and(|d| d.as_os_str().is_empty()) {
        std::path::Path::new(".").join(exe)
    } else {
        exe
    };
    let status = std::process::Command::new(&exe).args(args).status();
    let _ = std::fs::remove_dir_all(&tmp);
    match status {
        Ok(s) => match s.code() {
            Some(c) => ExitCode::from(c.clamp(0, 255) as u8),
            None => {
                eprintln!("error: the program was terminated abnormally ({s})");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("error: cannot run {}: {e}", exe.display());
            ExitCode::from(1)
        }
    }
}

fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Semi { implicit: true } => "Semi (newline)".to_string(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn report(file: &str, src: &str, errors: &[Diagnostic]) -> ExitCode {
    for e in errors {
        eprint!("{}", e.render(file, src));
    }
    if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "error: {} error{} in `{file}`",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        );
        ExitCode::from(1)
    }
}

fn mir(src: &str) -> Vec<Diagnostic> {
    let parsed = kiln_parser::parse(src);
    if !parsed.errors.is_empty() {
        return parsed.errors;
    }
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    if !resolved.errors.is_empty() {
        return resolved.errors;
    }
    let mut typed = kiln_typeck::typeck(&resolved);
    if !typed.errors().is_empty() {
        return typed.errors().to_vec();
    }
    for (_, bodies) in kiln_mir::lower_all(&mut typed) {
        for b in bodies {
            println!("{b}");
        }
    }
    vec![]
}
