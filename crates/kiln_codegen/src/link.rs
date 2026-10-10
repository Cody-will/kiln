//! Linking object files into executables with the system linker.
//!
//! * Linux / macOS: the C compiler driver (`cc`, or `$KILN_CC` / `$CC`).
//! * Windows (MSVC): `link.exe`, found with `vswhere` through the `cc` crate's
//!   registry lookup (the same way `rustc` finds it).

use std::path::Path;
use std::process::Command;

/// Links `object` with the runtime library into the executable `out`.
pub fn link(object: &Path, out: &Path) -> Result<(), String> {
    let dir = object.parent().unwrap_or(Path::new("."));
    let rt = dir.join(kiln_runtime::LIB_NAME);
    std::fs::write(&rt, kiln_runtime::LIB)
        .map_err(|e| format!("could not write {}: {e}", rt.display()))?;
    let mut cmd = linker_command(object, &rt, out)?;
    let output = cmd
        .output()
        .map_err(|e| format!("could not run the linker ({cmd:?}): {e}"))?;
    let _ = std::fs::remove_file(&rt);
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
fn linker_command(object: &Path, rt: &Path, out: &Path) -> Result<Command, String> {
    let cc = std::env::var("KILN_CC")
        .or_else(|_| std::env::var("CC"))
        .unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(cc);
    cmd.arg(object).arg(rt).arg("-o").arg(out).arg("-lm");
    Ok(cmd)
}

#[cfg(windows)]
fn linker_command(object: &Path, rt: &Path, out: &Path) -> Result<Command, String> {
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
    Ok(cmd)
}
