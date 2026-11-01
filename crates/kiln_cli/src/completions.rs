//! `kiln completions <bash|zsh|fish>`: shell completion scripts.

use std::process::ExitCode;

/// Subcommands offered at the first position, with a one-line summary.
pub const COMMANDS: &[(&str, &str)] = &[
    ("new", "Create a package"),
    ("add", "Add a dependency to kiln.toml"),
    ("update", "Update registry dependencies"),
    ("publish", "Add a library to a package index"),
    ("check", "Type- and borrow-check"),
    ("build", "Compile to a native executable"),
    ("run", "Build and run"),
    ("test", "Run @test functions"),
    ("fmt", "Format .kn files"),
    ("lsp", "The language server"),
    ("doctor", "Check the toolchain"),
    ("version", "Show the version"),
    ("editor", "Install editor support"),
    ("self", "Update or uninstall kiln"),
    ("completions", "Print a shell completion script"),
    ("help", "Show help"),
];

/// Options and arguments per subcommand.
const ARGS: &[(&str, &str)] = &[
    ("new", "--lib"),
    ("add", "--path"),
    ("publish", "--index --dry-run --init-index"),
    ("build", "--release --locked --offline --compiler=kilnc --compiler=rust --backend=llvm --backend=cranelift -o"),
    ("run", "--release --locked --offline --"),
    ("test", "--release --run --ignored --timeout"),
    ("fmt", "--check --stdin"),
    ("version", "-v --verbose"),
    ("editor", "install"),
    ("self", "update uninstall"),
    ("completions", "bash zsh fish"),
];

pub fn run(args: &[String]) -> ExitCode {
    match args {
        [s] if s == "bash" => print!("{}", bash()),
        [s] if s == "zsh" => print!("{}", zsh()),
        [s] if s == "fish" => print!("{}", fish()),
        _ => {
            eprintln!("usage: kiln completions <bash|zsh|fish>");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn names() -> String {
    COMMANDS.iter().map(|c| c.0).collect::<Vec<_>>().join(" ")
}

fn bash() -> String {
    let mut cases = String::new();
    for (c, a) in ARGS {
        cases.push_str(&format!("\t{c}) opts=\"{a}\" ;;\n"));
    }
    format!(
        r#"# bash completion for kiln (kiln completions bash)
_kiln() {{
	local cur cmd opts
	cur="${{COMP_WORDS[COMP_CWORD]}}"
	if [ "$COMP_CWORD" -eq 1 ]; then
		COMPREPLY=($(compgen -W "{names}" -- "$cur"))
		return
	fi
	cmd="${{COMP_WORDS[1]}}"
	opts=""
	case "$cmd" in
{cases}	esac
	if [ "$cmd" = editor ] && [ "$COMP_CWORD" -eq 3 ]; then
		opts="vscode nvim"
	elif [ "$cmd" = self ] && [ "$COMP_CWORD" -ge 3 ]; then
		opts="--version --dry-run --yes"
	fi
	case "$cmd" in
	completions|self|editor) COMPREPLY=($(compgen -W "$opts" -- "$cur")) ;;
	*) COMPREPLY=($(compgen -W "$opts" -- "$cur") $(compgen -f -X '!*.kn' -- "$cur") $(compgen -d -- "$cur")) ;;
	esac
}}
complete -o filenames -F _kiln kiln
"#,
        names = names()
    )
}

fn zsh() -> String {
    let mut cmds = String::new();
    for (c, d) in COMMANDS {
        cmds.push_str(&format!("\t\t'{c}:{d}'\n"));
    }
    let mut cases = String::new();
    for (c, a) in ARGS {
        cases.push_str(&format!("\t\t{c}) opts=({a}) ;;\n"));
    }
    format!(
        r#"#compdef kiln
# zsh completion for kiln (kiln completions zsh)
_kiln() {{
	local -a cmds opts
	cmds=(
{cmds}	)
	if (( CURRENT == 2 )); then
		_describe 'command' cmds
		return
	fi
	case $words[2] in
{cases}	esac
	if [[ $words[2] == editor && CURRENT -eq 4 ]]; then
		opts=(vscode nvim)
	elif [[ $words[2] == self && CURRENT -ge 4 ]]; then
		opts=(--version --dry-run --yes)
	fi
	case $words[2] in
		completions|self|editor) compadd -a opts ;;
		*) compadd -a opts; _files -g '*.kn' ;;
	esac
}}
compdef _kiln kiln
"#
    )
}

fn fish() -> String {
    let mut s =
        String::from("# fish completion for kiln (kiln completions fish)\ncomplete -c kiln -f\n");
    for (c, d) in COMMANDS {
        s.push_str(&format!(
            "complete -c kiln -n __fish_use_subcommand -a {c} -d '{d}'\n"
        ));
    }
    for (c, a) in ARGS {
        let words: Vec<&str> = a.split(' ').collect();
        let (flags, plain): (Vec<&str>, Vec<&str>) = words.iter().partition(|w| w.starts_with('-'));
        if !plain.is_empty() {
            s.push_str(&format!(
                "complete -c kiln -n '__fish_seen_subcommand_from {c}' -a '{}'\n",
                plain.join(" ")
            ));
        }
        for f in flags {
            if let Some(long) = f.strip_prefix("--") {
                let long = long.split('=').next().unwrap_or(long);
                if !long.is_empty() {
                    s.push_str(&format!(
                        "complete -c kiln -n '__fish_seen_subcommand_from {c}' -l {long}\n"
                    ));
                }
            } else if let Some(short) = f.strip_prefix('-') {
                s.push_str(&format!(
                    "complete -c kiln -n '__fish_seen_subcommand_from {c}' -s {short}\n"
                ));
            }
        }
    }
    s.push_str("complete -c kiln -n '__fish_seen_subcommand_from editor; and __fish_seen_subcommand_from install' -a 'vscode nvim'\n");
    s.push_str(
        "complete -c kiln -n '__fish_seen_subcommand_from self' -l version -l dry-run -l yes\n",
    );
    s.push_str("complete -c kiln -n '__fish_seen_subcommand_from check build run test fmt' -F -a '(__fish_complete_suffix .kn)'\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_completed() {
        for (c, _) in COMMANDS {
            assert!(bash().contains(c) && zsh().contains(c) && fish().contains(c));
        }
        for (c, _) in ARGS {
            assert!(COMMANDS.iter().any(|x| x.0 == *c), "{c}");
        }
    }
}
