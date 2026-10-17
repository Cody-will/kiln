//! Linking object files into executables with the system linker.
//!
//! * Linux / macOS: the C compiler driver (`cc`, or `$KILN_CC` / `$CC`).
//! * Windows (MSVC): `link.exe`, found with `vswhere` through the `cc` crate's
//!   registry lookup (the same way `rustc` finds it).

pub use kiln_parser::manifest::NativeLink;
use std::path::Path;
use std::process::Command;

/// Links `object` with the runtime library into the executable `out`.
pub fn link(object: &Path, out: &Path) -> Result<(), String> {
    link_with(object, out, &NativeLink::default())
}

/// [`link`], adding native libraries and C sources (`[link]` in kiln.toml).
pub fn link_with(object: &Path, out: &Path, native: &NativeLink) -> Result<(), String> {
    let dir = object.parent().unwrap_or(Path::new("."));
    // An installed kiln links the runtime from `<prefix>/lib/kiln/runtime`;
    // otherwise the copy built into this executable is written out.
    let installed = kiln_resolve::lib_dir().map(|d| d.join("runtime"));
    let inst = |name: &str| {
        installed
            .as_ref()
            .map(|d| d.join(name))
            .filter(|p| p.is_file())
    };
    let (rt, rt_tmp) = match inst(kiln_runtime::LIB_NAME) {
        Some(p) => (p, false),
        None => {
            let rt = dir.join(kiln_runtime::LIB_NAME);
            std::fs::write(&rt, kiln_runtime::LIB)
                .map_err(|e| format!("could not write {}: {e}", rt.display()))?;
            (rt, true)
        }
    };
    // The TLS library is only written and linked for programs that use it
    // (their object refers to `kn_tls_*` symbols).
    let uses_tls = std::fs::read(object)
        .map(|b| b.windows(7).any(|w| w == b"kn_tls_"))
        .unwrap_or(false);
    let (tls, tls_tmp) = match inst(kiln_runtime::TLS_LIB_NAME) {
        Some(p) => (p, false),
        None => (dir.join(kiln_runtime::TLS_LIB_NAME), true),
    };
    if uses_tls && tls_tmp {
        std::fs::write(&tls, kiln_runtime::TLS_LIB)
            .map_err(|e| format!("could not write {}: {e}", tls.display()))?;
    }
    let mut cmd = linker_command(object, &rt, uses_tls.then_some(tls.as_path()), out)?;
    add_native(&mut cmd, native)?;
    let output = cmd
        .output()
        .map_err(|e| format!("could not run the linker ({cmd:?}): {e}"))?;
    if rt_tmp {
        let _ = std::fs::remove_file(&rt);
    }
    if uses_tls && tls_tmp {
        let _ = std::fs::remove_file(&tls);
    }
    if !output.status.success() {
        return Err(format!(
            "linking failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn linker_command(
    object: &Path,
    rt: &Path,
    tls: Option<&Path>,
    out: &Path,
) -> Result<Command, String> {
    let cc = std::env::var("KILN_CC")
        .or_else(|_| std::env::var("CC"))
        .unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(cc);
    cmd.arg(object).arg(rt);
    if let Some(tls) = tls {
        cmd.arg(tls);
        // What the Rust standard library inside it needs.
        if cfg!(target_os = "macos") {
            cmd.args(["-lSystem", "-liconv"]);
        } else {
            cmd.args(["-lpthread", "-ldl", "-lrt", "-lutil", "-lgcc_s"]);
        }
    }
    cmd.arg("-o").arg(out).arg("-lm");
    Ok(cmd)
}

/// Native inputs go after Kiln's objects: C sources (the `cc` driver
/// compiles them), then search paths and libraries.
#[cfg(not(windows))]
fn add_native(cmd: &mut Command, n: &NativeLink) -> Result<(), String> {
    for c in &n.c_sources {
        if !c.is_file() {
            return Err(format!("[link] c-sources: {} not found", c.display()));
        }
        cmd.arg(c);
    }
    for d in &n.search {
        cmd.arg(format!("-L{}", d.display()));
    }
    for l in &n.libs {
        cmd.arg(format!("-l{l}"));
    }
    Ok(())
}

#[cfg(windows)]
fn add_native(cmd: &mut Command, n: &NativeLink) -> Result<(), String> {
    if !n.c_sources.is_empty() {
        return Err(
            "[link] c-sources are not supported on Windows yet; build a .lib and list it in `libs`"
                .into(),
        );
    }
    for d in &n.search {
        cmd.arg(format!("/LIBPATH:{}", d.display()));
    }
    for l in &n.libs {
        cmd.arg(format!("{l}.lib"));
    }
    Ok(())
}

#[cfg(windows)]
fn linker_command(
    object: &Path,
    rt: &Path,
    tls: Option<&Path>,
    out: &Path,
) -> Result<Command, String> {
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-pc-windows-msvc"
    } else {
        "x86_64-pc-windows-msvc"
    };
    let tool = cc::windows_registry::find_tool(target, "link.exe")
        .ok_or("could not find link.exe; install the Visual Studio C++ build tools")?;
    let mut cmd = tool.to_command();
    cmd.arg("/NOLOGO")
        .arg(format!("/OUT:{}", out.display()))
        .arg(object)
        .arg(rt)
        .arg("/SUBSYSTEM:CONSOLE")
        .arg("msvcrt.lib")
        .arg("ucrt.lib")
        .arg("vcruntime.lib")
        .arg("legacy_stdio_definitions.lib")
        .arg("kernel32.lib");
    if let Some(tls) = tls {
        cmd.arg(tls).args([
            "advapi32.lib",
            "ntdll.lib",
            "userenv.lib",
            "ws2_32.lib",
            "bcrypt.lib",
            "synchronization.lib",
        ]);
    }
    Ok(cmd)
}
