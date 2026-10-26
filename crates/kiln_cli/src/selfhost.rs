//! Building with kilnc, the self-hosted compiler (compiler/ in the source
//! tree). It is the **only** daily-driver code generator when installed:
//! `<prefix>/lib/kiln/kilnc`, `kilnc` next to this executable, or `$KILNC`.
//!
//! `--compiler=rust` / `KILN_COMPILER=rust` is **deprecated**: it selects the
//! Rust-built-in codegen for differential tests and bootstrap stage 1. Prefer
//! kilnc. `--backend=` and packages with native `[link]` still force the
//! built-in path (kilnc does not link natives yet). Path `[dependencies]`
//! are resolved by kilnc.
//!
//! When kilnc fails, `kiln` no longer silently falls back unless
//! `KILN_ALLOW_RUST_FALLBACK=1` is set (temporary escape hatch).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

/// 0: default (kilnc when available), 1: kilnc, 2: the built-in compiler.
static CHOICE: AtomicU8 = AtomicU8::new(0);

/// Applies `--compiler=<name>`; false if `a` is not one.
pub fn flag(a: &str) -> bool {
    match a {
        "--compiler=kilnc" => CHOICE.store(1, Ordering::Relaxed),
        "--compiler=rust" => {
            eprintln!(
                "warning: --compiler=rust is deprecated; kilnc is the default code generator (see docs/SELF_HOSTING.md)"
            );
            CHOICE.store(2, Ordering::Relaxed);
        }
        _ => return false,
    }
    true
}

/// Where kilnc is, if it is installed.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KILNC") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let name = format!("kilnc{}", std::env::consts::EXE_SUFFIX);
    let next = exe.parent()?.join(&name);
    if next.is_file() {
        return Some(next);
    }
    kiln_resolve::lib_dir()
        .map(|d| d.join(&name))
        .filter(|p| p.is_file())
}

/// Whether this build should use kilnc (and which one).
pub fn wanted(explicit_backend: bool, has_native: bool) -> Option<PathBuf> {
    let choice = match CHOICE.load(Ordering::Relaxed) {
        0 => match std::env::var("KILN_COMPILER").as_deref() {
            Ok("rust") => {
                eprintln!(
                    "warning: KILN_COMPILER=rust is deprecated; kilnc is the default code generator (see docs/SELF_HOSTING.md)"
                );
                2
            }
            Ok("kilnc") => 1,
            _ => 0,
        },
        c => c,
    };
    if choice == 2 || (choice == 0 && (explicit_backend || has_native)) {
        return None;
    }
    let k = find();
    if k.is_none() && choice == 1 {
        eprintln!("warning: --compiler=kilnc given but kilnc is not installed; using the built-in compiler");
    }
    k
}

/// A directory with kilnc's inputs from this `kiln`: the std sources in the
/// source-tree layout kilnc reads (`src/prelude.kn`, `std/<name>.kn`) and the
/// runtime libraries. Written once per kiln version and contents.
fn support_dir() -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kiln_resolve::PRELUDE_SRC.hash(&mut h);
    for (p, t) in kiln_resolve::STD_MODULES {
        p.hash(&mut h);
        t.hash(&mut h);
    }
    kiln_runtime::LIB.hash(&mut h);
    let dir = std::env::temp_dir().join(format!(
        "kiln-{}-{:016x}",
        env!("CARGO_PKG_VERSION"),
        h.finish()
    ));
    if dir.join("ok").is_file() {
        return Ok(dir);
    }
    // Build it under a unique name, then rename (concurrent builds race safely).
    let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
    let w = || -> std::io::Result<()> {
        std::fs::create_dir_all(tmp.join("src"))?;
        std::fs::create_dir_all(tmp.join("std"))?;
        std::fs::create_dir_all(tmp.join("rt"))?;
        std::fs::write(tmp.join("src/prelude.kn"), kiln_resolve::PRELUDE_SRC)?;
        for (p, t) in kiln_resolve::STD_MODULES {
            let last = p.rsplit("::").next().unwrap_or(p);
            std::fs::write(tmp.join("std").join(format!("{last}.kn")), t)?;
        }
        std::fs::write(
            tmp.join("rt").join(kiln_runtime::LIB_NAME),
            kiln_runtime::LIB,
        )?;
        std::fs::write(
            tmp.join("rt").join(kiln_runtime::TLS_LIB_NAME),
            kiln_runtime::TLS_LIB,
        )?;
        std::fs::write(tmp.join("ok"), "")?;
        Ok(())
    };
    w().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if std::fs::rename(&tmp, &dir).is_err() {
        // Another build got there first.
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(dir)
}

/// Builds `root` into `out` with kilnc. Intermediate files (LLVM IR shards,
/// objects, the borrow-check cache) go to `work`, so rebuilds are
/// incremental. `Err` carries kilnc's output.
pub fn build(
    kilnc: &Path,
    root: &Path,
    out: &Path,
    work: &Path,
    release: bool,
) -> Result<(), String> {
    let sup = support_dir()?;
    std::fs::create_dir_all(work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let stem = out
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "a.out".into());
    let tmp_out = work.join(&stem);
    let mut cmd = std::process::Command::new(kilnc);
    cmd.arg("--build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg(root).arg("-o").arg(&tmp_out);
    cmd.env("KILN_RT", sup.join("rt").join(kiln_runtime::LIB_NAME));
    cmd.env("KILNC_RESOLVE_DIR", &sup);
    if std::env::var_os("KILN_CLANG").is_none() {
        if let Ok(cc) = std::env::var("KILN_CC") {
            cmd.env("KILN_CLANG", cc);
        }
    }
    let o = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", kilnc.display()))?;
    if !o.status.success() {
        let mut s = String::from_utf8_lossy(&o.stderr).into_owned();
        s.push_str(&String::from_utf8_lossy(&o.stdout));
        return Err(s);
    }
    std::fs::copy(&tmp_out, out)
        .map(|_| ())
        .map_err(|e| format!("cannot write {}: {e}", out.display()))
}

static EXPLICIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Records whether `--backend=` was given (which selects the built-in compiler).
pub fn set_explicit_backend(b: bool) {
    EXPLICIT.store(b, Ordering::Relaxed);
}

pub fn explicit_backend() -> bool {
    EXPLICIT.load(Ordering::Relaxed)
}
