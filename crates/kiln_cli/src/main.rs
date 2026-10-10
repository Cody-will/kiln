//! The `kiln` command-line driver.
//!
//! Currently supports inspecting the front end:
//!
//! - `kiln lex <file>`: print tokens with line:column positions.
//! - `kiln parse <file>`: print the AST.
//! - `kiln check <file>`: parse, resolve names, type-check, and borrow-check; print diagnostics.
//! - `kiln mir <file>`: print the lowered MIR (for compiler debugging).
//! - `kiln build <file> [-o <output>]`: compile to a native executable.
//! - `kiln run <file> [args...]`: build to a temporary executable and run it.
//!
//! Exit status is 0 on success, 1 when the file has errors, and 2 on usage errors
//! (`kiln run` exits with the program's status).

use kiln_lexer::{lex, Diagnostic, LineIndex, TokenKind};
use std::process::ExitCode;

const USAGE: &str = "\
kiln - the Kiln language toolchain

USAGE:
    kiln lex <file.kn>      Print the token stream
    kiln parse <file.kn>    Print the syntax tree
    kiln check <file.kn>    Resolve names, type-check, and borrow-check
    kiln mir <file.kn>      Print the mid-level IR (compiler debugging)
    kiln build <file.kn> [-o <output>]
                            Compile to a native executable
    kiln run <file.kn> [args...]
                            Build and run
    kiln help               Show this message
    kiln --version          Show the version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, file] if cmd == "lex" || cmd == "parse" || cmd == "check" || cmd == "mir" => {
            run(cmd, file)
        }
        [cmd, file, rest @ ..] if cmd == "build" => match rest {
            [] => build(file, None),
            [o, out] if o == "-o" => build(file, Some(out)),
            _ => {
                eprintln!("{USAGE}");
                ExitCode::from(2)
            }
        },
        [cmd, file, rest @ ..] if cmd == "run" => run_program(file, rest),
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
    } else if cmd == "check" {
        let errors = kiln_borrowck::check_source(&src);
        if errors.is_empty() {
            println!("{file}: ok");
        }
        errors
    } else if cmd == "mir" {
        mir(&src)
    } else {
        let parsed = kiln_parser::parse(&src);
        println!("{:#?}", parsed.file);
        parsed.errors
    };
    report(file, &src, &errors)
}

fn read(file: &str) -> Option<String> {
    match std::fs::read_to_string(file) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("error: cannot read `{file}`: {e}");
            None
        }
    }
}

fn default_output(file: &str) -> std::path::PathBuf {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a");
    std::path::PathBuf::from(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

fn build_to(file: &str, src: &str, out: &std::path::Path) -> Result<(), ExitCode> {
    match kiln_codegen::build_source(src, out) {
        Ok(()) => Ok(()),
        Err(kiln_codegen::BuildError::Diagnostics(errs)) => Err(report(file, src, &errs)),
        Err(kiln_codegen::BuildError::Other(msg)) => {
            eprintln!("error: {msg}");
            Err(ExitCode::from(1))
        }
    }
}

fn build(file: &str, out: Option<&String>) -> ExitCode {
    let Some(src) = read(file) else {
        return ExitCode::from(2);
    };
    let out = out
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_output(file));
    match build_to(file, &src, &out) {
        Ok(()) => {
            println!("built {}", out.display());
            ExitCode::SUCCESS
        }
        Err(c) => c,
    }
}

fn run_program(file: &str, args: &[String]) -> ExitCode {
    let Some(src) = read(file) else {
        return ExitCode::from(2);
    };
    let dir = std::env::temp_dir().join(format!("kiln-run-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return ExitCode::from(1);
    }
    let exe = dir.join(default_output(file));
    if let Err(c) = build_to(file, &src, &exe) {
        let _ = std::fs::remove_dir_all(&dir);
        return c;
    }
    let status = std::process::Command::new(&exe).args(args).status();
    let _ = std::fs::remove_dir_all(&dir);
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

fn report(file: &str, src: &str, errors: &[Diagnostic]) -> ExitCode {
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
