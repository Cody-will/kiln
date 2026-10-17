//! `kiln version`, `kiln doctor`, and the installer's `kiln __install-lib`.

use std::path::Path;
use std::process::{Command, ExitCode};

/// Where std and the runtime come from.
fn lib_source() -> String {
    match kiln_resolve::lib_dir() {
        Some(d) => d.display().to_string(),
        None => "built in (no <prefix>/lib/kiln next to this executable)".into(),
    }
}

pub fn version(verbose: bool) -> ExitCode {
    println!("kiln {}", env!("CARGO_PKG_VERSION"));
    if verbose {
        if let Ok(exe) = std::env::current_exe() {
            println!("executable: {}", exe.display());
        }
        println!("library:    {}", lib_source());
        println!(
            "backends:   cranelift{}",
            if kiln_codegen::llvm_available() {
                ", llvm"
            } else {
                ""
            }
        );
        println!(
            "target:     {}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
    }
    ExitCode::SUCCESS
}

/// Checks that programs can be built here: the C compiler driver used as
/// the linker, the library, and an actual hello-world build and run.
pub fn doctor() -> ExitCode {
    let mut ok = true;
    let mut check = |what: &str, r: Result<String, String>| match r {
        Ok(s) => println!("ok    {what}: {s}"),
        Err(e) => {
            ok = false;
            println!("FAIL  {what}: {e}");
        }
    };
    check(
        "kiln",
        Ok(format!("{} ({})", env!("CARGO_PKG_VERSION"), lib_source())),
    );
    let cc = std::env::var("KILN_CC")
        .or_else(|_| std::env::var("CC"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "link.exe".into()
            } else {
                "cc".into()
            }
        });
    let cc_res = if cfg!(windows) {
        Ok(format!("{cc} (found through the Visual Studio installer)"))
    } else {
        match Command::new(&cc).arg("--version").output() {
            Ok(o) if o.status.success() => Ok(format!(
                "{cc}: {}",
                String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim()
            )),
            Ok(o) => Err(format!("`{cc} --version` failed ({})", o.status)),
            Err(_) => Err(format!(
                "no C compiler driver `{cc}` on PATH; kiln links with it. Install one \
                 (Debian/Ubuntu: `sudo apt install build-essential`; Fedora: `sudo dnf install gcc`; \
                 macOS: `xcode-select --install`) or set KILN_CC"
            )),
        }
    };
    let have_cc = cc_res.is_ok();
    check("C toolchain / linker", cc_res);
    if have_cc {
        check("build and run hello world", hello());
    }
    if ok {
        println!("\nkiln is ready.");
        ExitCode::SUCCESS
    } else {
        println!("\nfix the failures above, then run `kiln doctor` again.");
        ExitCode::from(1)
    }
}

fn hello() -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("kiln-doctor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let src = dir.join("hello.kn");
    std::fs::write(&src, "fun main() {\n\tprintln(\"hello from kiln\")\n}\n")
        .map_err(|e| e.to_string())?;
    let exe = dir.join("hello");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let b = Command::new(me)
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| e.to_string())?;
    let r = if !b.status.success() {
        Err(format!(
            "build failed:\n{}",
            String::from_utf8_lossy(&b.stderr)
        ))
    } else {
        match Command::new(&exe).output() {
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "hello from kiln" => {
                Ok("ok".into())
            }
            Ok(o) => Err(format!(
                "unexpected output {:?}",
                String::from_utf8_lossy(&o.stdout)
            )),
            Err(e) => Err(format!("could not run the program: {e}")),
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// Writes std and the runtime libraries into `dir` (the installer's
/// `<prefix>/lib/kiln`), in the layout `kiln_resolve::lib_dir` reads.
pub fn install_lib(dir: &Path) -> ExitCode {
    let write = |rel: &str, bytes: &[u8]| -> Result<(), String> {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))
    };
    let mut r = write("std/prelude.kn", kiln_resolve::PRELUDE_SRC.as_bytes());
    for (path, src) in kiln_resolve::STD_MODULES {
        r = r.and_then(|_| {
            write(
                &format!("std/{}", kiln_resolve::std_file(path)),
                src.as_bytes(),
            )
        });
    }
    r = r
        .and_then(|_| {
            write(
                &format!("runtime/{}", kiln_runtime::LIB_NAME),
                kiln_runtime::LIB,
            )
        })
        .and_then(|_| {
            write(
                &format!("runtime/{}", kiln_runtime::TLS_LIB_NAME),
                kiln_runtime::TLS_LIB,
            )
        });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
