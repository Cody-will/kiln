//! Packages: `kiln.toml`, the `src/` layout, path dependencies, and
//! `kiln new`.
//!
//! ```toml
//! [package]
//! name = "hello"
//! version = "0.1.0"
//!
//! [dependencies]
//! util = { path = "../util" }
//! ```
//!
//! A package's root file is `src/main.kn` (a program) or `src/lib.kn` (a
//! library). A dependency is a library package; it becomes a top-level
//! module named after the dependency. Builds go to `target/debug/<name>`
//! (or `target/release/`) and are skipped when nothing changed.

use kiln_parser::manifest::{all_deps, parse_manifest};
pub use kiln_parser::manifest::{find_manifest, Manifest};
use kiln_parser::package::{self, Package};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// A program to compile: the loaded package and how to name it.
pub struct Program {
    /// The loaded files.
    pub pkg: Package,
    /// The root file's path, for messages.
    pub root: PathBuf,
    /// The package manifest, if this is a package.
    pub manifest: Option<Manifest>,
}

impl Program {
    /// The root file's text.
    pub fn src(&self) -> &str {
        &self.pkg.files[0].2
    }

    /// The root file's display name.
    pub fn name(&self) -> String {
        self.root.display().to_string()
    }
}

/// Loads a program: `target` is a `.kn` file, a package directory, or
/// `None` for the package containing the current directory. `lib_ok`
/// accepts a library package (`src/lib.kn`), for check and test.
pub fn load(target: Option<&str>, lib_ok: bool) -> Result<Program, ExitCode> {
    let fail = |msg: String| {
        eprintln!("error: {msg}");
        ExitCode::from(2)
    };
    let (root, manifest) = match target {
        Some(t) if !Path::new(t).is_dir() => {
            let p = PathBuf::from(t);
            // A file inside a package still sees the package's dependencies.
            let m = find_manifest(
                p.parent()
                    .filter(|d| !d.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )
            .map(|m| parse_manifest(&m))
            .transpose()
            .map_err(fail)?;
            (p, m)
        }
        _ => {
            let dir = PathBuf::from(target.unwrap_or("."));
            let Some(mpath) = find_manifest(&dir) else {
                return Err(fail(format!(
                    "no kiln.toml in {} or its parents (give a .kn file, or create a package with `kiln new`)",
                    dir.display()
                )));
            };
            let m = parse_manifest(&mpath).map_err(fail)?;
            let main = m.dir.join("src").join("main.kn");
            let lib = m.dir.join("src").join("lib.kn");
            let root = if main.is_file() {
                main
            } else if lib_ok && lib.is_file() {
                lib
            } else if lib.is_file() {
                return Err(fail(format!(
                    "package `{}` is a library (src/lib.kn); there is nothing to run",
                    m.name
                )));
            } else {
                return Err(fail(format!(
                    "package `{}` has no src/main.kn or src/lib.kn",
                    m.name
                )));
            };
            let root = pathdiff(&root);
            (root, Some(m))
        }
    };
    let deps = match &manifest {
        Some(m) => all_deps(m).map_err(fail)?,
        None => vec![],
    };
    let pkg = package::load(&root, &deps, &|_| None).map_err(fail)?;
    if let Some(m) = &manifest {
        sync_lock(m, &deps, &pkg).map_err(fail)?;
    }
    Ok(Program {
        pkg,
        root,
        manifest,
    })
}

// ----- kiln.lock ---------------------------------------------------------------

static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--locked`: fail instead of updating `kiln.lock`.
pub fn set_locked() {
    LOCKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// FNV-1a, 64-bit: stable across compiler versions (unlike `DefaultHasher`).
fn fnv64(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// The text of `kiln.lock` for a package: every dependency (transitively)
/// with its version, its source, and a checksum of the files it
/// contributes to the build.
pub fn lock_text(
    m: &Manifest,
    deps: &[package::Dependency],
    pkg: &Package,
) -> Result<String, String> {
    let mut entries = vec![];
    for d in deps {
        let dir = d
            .root
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."));
        let dm = parse_manifest(&dir.join("kiln.toml"))?;
        let canon = dir.canonicalize().unwrap_or(dir.to_path_buf());
        let mut files: Vec<(String, &str)> = pkg
            .files
            .iter()
            .filter_map(|(_, p, text)| {
                let c = p.canonicalize().ok()?;
                let rel = c.strip_prefix(&canon).ok()?;
                Some((rel.to_string_lossy().replace('\\', "/"), text.as_str()))
            })
            .collect();
        files.sort();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (rel, text) in &files {
            fnv64(&mut h, rel.as_bytes());
            fnv64(&mut h, &[0]);
            fnv64(&mut h, text.as_bytes());
            fnv64(&mut h, &[0]);
        }
        let src = match canon.strip_prefix(m.dir.canonicalize().unwrap_or(m.dir.clone())) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => pathdiff_from(&canon, &m.dir),
        };
        entries.push((d.name.clone(), dm.version.clone(), src, format!("{h:016x}")));
    }
    entries.sort();
    let mut out = String::from(
        "# This file is generated by kiln: the exact dependencies of this package.\n# Commit it; `kiln build --locked` fails if it would change.\nversion = 1\n",
    );
    for (name, version, src, sum) in entries {
        out.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"path+{src}\"\nchecksum = \"fnv64:{sum}\"\n"
        ));
    }
    Ok(out)
}

/// A relative path from `base` to `target` (both absolute).
fn pathdiff_from(target: &Path, base: &Path) -> String {
    let base = base.canonicalize().unwrap_or(base.to_path_buf());
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".into(); b.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Writes `kiln.lock` when it is missing or out of date (packages with
/// dependencies only), or with `--locked`, reports how it differs.
fn sync_lock(m: &Manifest, deps: &[package::Dependency], pkg: &Package) -> Result<(), String> {
    let path = m.dir.join("kiln.lock");
    let old = std::fs::read_to_string(&path).ok();
    if deps.is_empty() && old.is_none() {
        return Ok(());
    }
    let new = lock_text(m, deps, pkg)?;
    if old.as_deref() == Some(new.as_str()) {
        return Ok(());
    }
    if LOCKED.load(std::sync::atomic::Ordering::Relaxed) {
        let changed = match &old {
            None => "it is missing".to_string(),
            Some(o) => {
                let a: Vec<&str> = o
                    .lines()
                    .filter(|l| {
                        l.starts_with("name")
                            || l.starts_with("version =")
                            || l.starts_with("checksum")
                            || l.starts_with("source")
                    })
                    .collect();
                let b: Vec<&str> = new
                    .lines()
                    .filter(|l| {
                        l.starts_with("name")
                            || l.starts_with("version =")
                            || l.starts_with("checksum")
                            || l.starts_with("source")
                    })
                    .collect();
                let diff: Vec<String> = b
                    .iter()
                    .filter(|l| !a.contains(l))
                    .map(|l| format!("now {l}"))
                    .chain(
                        a.iter()
                            .filter(|l| !b.contains(l))
                            .map(|l| format!("was {l}")),
                    )
                    .collect();
                diff.join("; ")
            }
        };
        return Err(format!(
            "kiln.lock is out of date ({changed}); run without --locked to update it"
        ));
    }
    std::fs::write(&path, new).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `path` relative to the current directory when it is inside it.
fn pathdiff(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    match (cwd, path.canonicalize()) {
        (Some(c), Ok(p)) => p.strip_prefix(&c).map(Path::to_path_buf).unwrap_or(p),
        _ => path.to_path_buf(),
    }
}

/// Where `kiln build` puts a package's executable.
pub fn output_path(m: &Manifest, release: bool) -> PathBuf {
    let dir = m
        .dir
        .join("target")
        .join(if release { "release" } else { "debug" });
    pathdiff(&dir).join(format!("{}{}", m.name, std::env::consts::EXE_SUFFIX))
}

/// A fingerprint of the inputs of a build: every file's text, the build
/// options, and the compiler itself. When it matches the one stored next to
/// the executable, the build is skipped.
pub fn fingerprint(p: &Program, release: bool, llvm: bool) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (_, path, text) in &p.pkg.files {
        path.hash(&mut h);
        text.hash(&mut h);
    }
    release.hash(&mut h);
    llvm.hash(&mut h);
    // Native inputs: the [link] settings and the C sources' contents.
    if let Some(n) = p
        .manifest
        .as_ref()
        .and_then(|m| kiln_parser::manifest::all_link(m).ok())
    {
        format!("{n:?}").hash(&mut h);
        for c in &n.c_sources {
            std::fs::read(c).unwrap_or_default().hash(&mut h);
        }
    }
    env!("CARGO_PKG_VERSION").hash(&mut h);
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(meta) = std::fs::metadata(exe) {
            meta.len().hash(&mut h);
            if let Ok(t) = meta.modified() {
                t.hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

/// `kiln new <name> [--lib]`.
pub fn new(args: &[String]) -> ExitCode {
    let mut name = None;
    let mut lib = false;
    for a in args {
        match a.as_str() {
            "--lib" => lib = true,
            s if s.starts_with('-') => {
                eprintln!("error: unknown option `{s}`\nusage: kiln new <name> [--lib]");
                return ExitCode::from(2);
            }
            s => name = Some(s.to_string()),
        }
    }
    let Some(path) = name else {
        eprintln!("usage: kiln new <name> [--lib]");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(&path);
    let pkg_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .replace('-', "_");
    if !kiln_parser::manifest::is_ident(&pkg_name) {
        eprintln!("error: `{pkg_name}` is not a valid package name (use letters, digits, and _)");
        return ExitCode::from(2);
    }
    if dir.exists() {
        eprintln!("error: `{}` already exists", dir.display());
        return ExitCode::from(1);
    }
    let files: Vec<(PathBuf, String)> = vec![
        (
            dir.join("kiln.toml"),
            format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n\n[dependencies]\n"),
        ),
        (dir.join(".gitignore"), "/target\n".into()),
        if lib {
            (
                dir.join("src/lib.kn"),
                "// Adds two numbers.\npub fun add(a i64, b i64) i64 {\n\treturn a + b\n}\n\n@test\nfun adds() {\n\tassert_eq(add(2, 2), 4)\n}\n".into(),
            )
        } else {
            (
                dir.join("src/main.kn"),
                "fun main() {\n\tprintln(\"Hello, world!\")\n}\n".into(),
            )
        },
    ];
    for (p, text) in files {
        if let Some(parent) = p.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {e}", parent.display());
                return ExitCode::from(1);
            }
        }
        if let Err(e) = std::fs::write(&p, text) {
            eprintln!("error: cannot write {}: {e}", p.display());
            return ExitCode::from(1);
        }
    }
    println!(
        "created {} package `{pkg_name}` in {}",
        if lib { "library" } else { "program" },
        dir.display()
    );
    ExitCode::SUCCESS
}
