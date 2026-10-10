//! The `kiln` command-line driver.
//!
//! Currently supports inspecting the front end:
//!
//! - `kiln lex <file>`: print tokens with line:column positions.
//! - `kiln parse <file>`: print the AST.
//!
//! Exit status is 0 on success, 1 when the file has errors, and 2 on usage errors.

use kiln_lexer::{lex, Diagnostic, LineIndex, TokenKind};
use std::process::ExitCode;

const USAGE: &str = "\
kiln - the Kiln language toolchain

USAGE:
    kiln lex <file.kn>      Print the token stream
    kiln parse <file.kn>    Print the syntax tree
    kiln help               Show this message
    kiln --version          Show the version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, file] if cmd == "lex" || cmd == "parse" => run(cmd, file),
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
    } else {
        let parsed = kiln_parser::parse(&src);
        println!("{:#?}", parsed.file);
        parsed.errors
    };
    report(file, &src, &errors)
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
