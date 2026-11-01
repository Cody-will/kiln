//! `kiln self update` and `kiln self uninstall`, for installs made by
//! `install.sh` (`<prefix>/bin/kiln` next to `<prefix>/lib/kiln`).
//!
//! `update` downloads the release archive the installer would (honouring
//! `KILN_DOWNLOAD_URL`, which may be a `file://` directory), checks its
//! `.sha256` when one is published, and swaps `bin/kiln` and `lib/kiln` in
//! place. Installs owned by a system package manager (`/usr`, `/usr/local`
//! with no write access, ...) are left to that package manager.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const REPO: &str = "Cody-will/kiln";
const MARKER: &str = "# added by the Kiln installer";

const USAGE: &str = "\
kiln self update [--version <tag>] [--dry-run]
                        Download the latest (or the given) release and replace
                        this installation (KILN_DOWNLOAD_URL overrides the
                        download location; a file:// URL works offline)
kiln self uninstall [--yes]
                        Remove this installation (bin/kiln, lib/kiln and the
                        PATH lines the installer added); without --yes, only
                        lists what would be removed";

pub fn run(args: &[String]) -> ExitCode {
    let r = match args {
        [cmd, rest @ ..] if cmd == "update" => update(rest),
        [cmd, rest @ ..] if cmd == "uninstall" => uninstall(rest),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// The install prefix of the running `kiln` (`KILN_SELF_PREFIX` overrides it
/// for tests), refusing package-manager installs.
fn prefix() -> Result<PathBuf, String> {
    let p = match std::env::var_os("KILN_SELF_PREFIX") {
        Some(p) => PathBuf::from(p),
        None => {
            let exe = std::env::current_exe().map_err(|e| format!("cannot find kiln: {e}"))?;
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            let bin = exe.parent().ok_or("kiln is not in a bin directory")?;
            if bin.file_name().is_none_or(|n| n != "bin") {
                return Err(format!(
                    "{} is not an installed kiln (<prefix>/bin/kiln); rebuild it from its checkout instead",
                    exe.display()
                ));
            }
            bin.parent().ok_or("kiln is not in a prefix")?.to_path_buf()
        }
    };
    if p == Path::new("/usr") {
        return Err(
            "this kiln is managed by the system package manager (e.g. pacman -Syu kiln-bin)".into(),
        );
    }
    if !p.join("lib/kiln").is_dir() {
        return Err(format!(
            "{} has no lib/kiln: not an install.sh installation",
            p.display()
        ));
    }
    Ok(p)
}

fn platform() -> Result<String, String> {
    if std::env::consts::OS != "linux" {
        return Err(format!(
            "no prebuilt Kiln for {} yet; update from a source checkout with ./install.sh",
            std::env::consts::OS
        ));
    }
    match std::env::consts::ARCH {
        "x86_64" | "aarch64" => Ok(format!("kiln-{}-linux.tar.gz", std::env::consts::ARCH)),
        a => Err(format!("no prebuilt Kiln for {a} yet")),
    }
}

fn fetch(url: &str, to: &Path) -> Result<(), String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::copy(path, to)
            .map(|_| ())
            .map_err(|e| format!("{url}: {e}"));
    }
    let to_s = to.to_string_lossy();
    let ok = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o", &to_s, url])
        .status()
        .or_else(|_| Command::new("wget").args(["-qO", &to_s, url]).status())
        .map_err(|_| "need curl or wget".to_string())?;
    if ok.success() {
        Ok(())
    } else {
        Err(format!("download failed: {url}"))
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let p = path.to_string_lossy();
    let out = Command::new("sha256sum")
        .arg(&*p)
        .output()
        .or_else(|_| Command::new("shasum").args(["-a", "256", &p]).output())
        .map_err(|_| "need sha256sum or shasum".to_string())?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "cannot checksum the download".to_string())
}

fn update(args: &[String]) -> Result<(), String> {
    let (mut version, mut dry) = (None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => version = Some(it.next().ok_or("--version needs a tag")?.clone()),
            "--dry-run" => dry = true,
            _ => return Err(format!("unknown option `{a}`\n{USAGE}")),
        }
    }
    let prefix = prefix()?;
    let asset = platform()?;
    let base = match (std::env::var("KILN_DOWNLOAD_URL"), &version) {
        (Ok(u), _) if !u.is_empty() => u.trim_end_matches('/').to_string(),
        (_, Some(v)) => format!("https://github.com/{REPO}/releases/download/{v}"),
        _ => format!("https://github.com/{REPO}/releases/latest/download"),
    };
    let url = format!("{base}/{asset}");
    if dry {
        println!("would download {url} into {}", prefix.display());
        return Ok(());
    }
    let tmp = std::env::temp_dir().join(format!("kiln-self-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("x")).map_err(|e| e.to_string())?;
    let r = update_from(&url, &tmp, &prefix);
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

fn update_from(url: &str, tmp: &Path, prefix: &Path) -> Result<(), String> {
    println!("Downloading {url} ...");
    let archive = tmp.join("kiln.tar.gz");
    fetch(url, &archive)?;
    let sum = tmp.join("kiln.tar.gz.sha256");
    if fetch(&format!("{url}.sha256"), &sum).is_ok() {
        let want = std::fs::read_to_string(&sum).unwrap_or_default();
        let want = want.split_whitespace().next().unwrap_or("");
        if want != sha256(&archive)? {
            return Err(format!("checksum mismatch for {url}"));
        }
    }
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(tmp.join("x"))
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !st.success() {
        return Err("cannot unpack the release archive".into());
    }
    let root = std::fs::read_dir(tmp.join("x"))
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .ok_or("the release archive is empty")?;
    if !root.join("bin/kiln").is_file() || !root.join("lib/kiln").is_dir() {
        return Err("the release archive has no bin/kiln and lib/kiln".into());
    }
    // Stage next to the install, then swap (rename is atomic per path; the
    // running executable stays valid until it exits).
    let (lib, lib_new, lib_old) = (
        prefix.join("lib/kiln"),
        prefix.join("lib/kiln.new"),
        prefix.join("lib/kiln.old"),
    );
    let (bin, bin_new) = (prefix.join("bin/kiln"), prefix.join("bin/kiln.new"));
    let _ = std::fs::remove_dir_all(&lib_new);
    let _ = std::fs::remove_dir_all(&lib_old);
    copy_dir(&root.join("lib/kiln"), &lib_new)
        .map_err(|e| format!("{}: {e}", lib_new.display()))?;
    std::fs::copy(root.join("bin/kiln"), &bin_new)
        .map_err(|e| format!("{}: {e}", bin_new.display()))?;
    std::fs::rename(&lib, &lib_old).map_err(|e| format!("{}: {e}", lib.display()))?;
    if let Err(e) = std::fs::rename(&lib_new, &lib) {
        let _ = std::fs::rename(&lib_old, &lib);
        return Err(format!("{}: {e}", lib.display()));
    }
    std::fs::rename(&bin_new, &bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let _ = std::fs::remove_dir_all(&lib_old);
    let v = Command::new(&bin)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("Installed {v} into {}.", prefix.display());
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_dir(&src, &dst)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn rc_files() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    [".profile", ".bashrc", ".zshrc"]
        .iter()
        .map(|f| home.join(f))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|s| s.lines().any(|l| l.contains(MARKER))))
        .collect()
}

fn uninstall(args: &[String]) -> Result<(), String> {
    let yes = match args {
        [] => false,
        [a] if a == "--yes" || a == "-y" => true,
        _ => return Err(format!("unknown option\n{USAGE}")),
    };
    let prefix = prefix()?;
    let fish = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".config/fish/conf.d/kiln.fish"))
        .filter(|p| p.is_file());
    let rcs = rc_files();
    if !yes {
        println!("kiln self uninstall --yes would remove:");
        println!("  {}", prefix.join("bin/kiln").display());
        println!("  {}", prefix.join("lib/kiln").display());
        for f in &rcs {
            println!("  the Kiln PATH line in {}", f.display());
        }
        if let Some(f) = &fish {
            println!("  {}", f.display());
        }
        return Ok(());
    }
    std::fs::remove_dir_all(prefix.join("lib/kiln"))
        .map_err(|e| format!("{}: {e}", prefix.join("lib/kiln").display()))?;
    let _ = std::fs::remove_file(prefix.join("bin/kiln"));
    for d in ["bin", "lib", ""] {
        let _ = std::fs::remove_dir(prefix.join(d));
    }
    for f in rcs {
        if let Ok(s) = std::fs::read_to_string(&f) {
            let mut kept = String::new();
            for l in s.lines().filter(|l| !l.contains(MARKER)) {
                kept.push_str(l);
                kept.push('\n');
            }
            let _ = std::fs::write(&f, kept);
        }
    }
    if let Some(f) = fish {
        let _ = std::fs::remove_file(f);
    }
    println!("Kiln removed from {}.", prefix.display());
    Ok(())
}
