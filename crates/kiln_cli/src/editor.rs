//! `kiln editor install <vscode|nvim>`: installs the editor support shipped
//! in `<prefix>/lib/kiln/editors` (or, in a source checkout, `editors/`).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The directory holding `kiln.vsix` and `nvim/`.
fn editors_dir() -> Option<PathBuf> {
    if let Some(d) = kiln_resolve::lib_dir().map(|d| d.join("editors")) {
        if d.is_dir() {
            return Some(d);
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors");
    src.is_dir()
        .then(|| std::fs::canonicalize(&src).unwrap_or(src))
}

pub fn run(args: &[String]) -> ExitCode {
    let mut code = None;
    let mut which = None;
    match args {
        [cmd, rest @ ..] if cmd == "install" => {
            for a in rest {
                if let Some(c) = a.strip_prefix("--code=") {
                    code = Some(c.to_string());
                } else if which.is_none() && !a.starts_with('-') {
                    which = Some(a.clone());
                } else {
                    return usage();
                }
            }
        }
        _ => return usage(),
    }
    let Some(dir) = editors_dir() else {
        eprintln!("error: no editor support found (expected <prefix>/lib/kiln/editors)");
        return ExitCode::from(1);
    };
    match which.as_deref() {
        Some("vscode") | Some("code") => vscode(&dir, code),
        Some("nvim") | Some("neovim") => nvim(&dir),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: kiln editor install <vscode|nvim> [--code=<code|codium|...>]");
    ExitCode::from(2)
}

fn vscode(dir: &Path, code: Option<String>) -> ExitCode {
    let vsix = [dir.join("kiln.vsix"), dir.join("vscode/kiln.vsix")]
        .into_iter()
        .find(|p| p.is_file());
    let Some(vsix) = vsix else {
        eprintln!(
            "error: {} has no kiln.vsix; in a source checkout run `make vsix` first",
            dir.display()
        );
        return ExitCode::from(1);
    };
    let cands = match code {
        Some(c) => vec![c],
        None => ["code", "codium", "code-insiders", "code-oss"]
            .map(String::from)
            .to_vec(),
    };
    for c in &cands {
        match Command::new(c)
            .arg("--install-extension")
            .arg(&vsix)
            .arg("--force")
            .status()
        {
            Ok(s) if s.success() => {
                println!("installed the Kiln extension into {c}");
                return ExitCode::SUCCESS;
            }
            Ok(s) => {
                eprintln!("error: `{c} --install-extension` failed ({s})");
                return ExitCode::from(1);
            }
            Err(_) => continue,
        }
    }
    eprintln!(
        "error: no VS Code CLI found (tried {}); run `code --install-extension {}` yourself",
        cands.join(", "),
        vsix.display()
    );
    ExitCode::from(1)
}

/// Copies the plugin into nvim's native package path, so it loads at
/// startup without a plugin manager.
fn nvim(dir: &Path) -> ExitCode {
    let src = dir.join("nvim");
    if !src.join("plugin").is_dir() {
        eprintln!("error: {} is not the Kiln nvim plugin", src.display());
        return ExitCode::from(1);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let Some(data) = data else {
        eprintln!("error: neither XDG_DATA_HOME nor HOME is set");
        return ExitCode::from(1);
    };
    let dest = data.join("nvim/site/pack/kiln/start/kiln.nvim");
    let _ = std::fs::remove_dir_all(&dest);
    if let Err(e) = copy_dir(&src, &dest) {
        eprintln!("error: cannot copy to {}: {e}", dest.display());
        return ExitCode::from(1);
    }
    println!("installed the Kiln nvim plugin into {}", dest.display());
    println!("then, in nvim: :TSInstall kiln   (needs nvim-treesitter and a C compiler)");
    ExitCode::SUCCESS
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}
