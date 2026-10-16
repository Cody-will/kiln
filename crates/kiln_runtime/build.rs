//! Compiles the C runtime into a static library that `kiln build` links into
//! every executable, and builds the TLS library (`crates/kiln_tls`, rustls
//! behind a C ABI) as a second static library, linked the same way.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    for f in [
        "kiln_rt.c",
        "kiln_sched.c",
        "kiln_std.c",
        "kiln_unicode.c",
        "kiln_math.c",
        "ryu/d2s.c",
    ] {
        println!("cargo:rerun-if-changed=runtime/{f}");
    }
    cc::Build::new()
        .file("runtime/kiln_rt.c")
        .file("runtime/kiln_sched.c")
        .file("runtime/kiln_std.c")
        .file("runtime/kiln_unicode.c")
        .file("runtime/kiln_math.c")
        .file("runtime/ryu/d2s.c")
        .include("runtime")
        .opt_level(2)
        .warnings(true)
        .cargo_metadata(false)
        .compile("kiln_rt");
    build_tls();
}

/// Builds `crates/kiln_tls` with a nested cargo invocation (it is a separate
/// workspace with its own lock file) and copies the archive into `OUT_DIR`.
fn build_tls() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let tls_dir = manifest_dir.join("../kiln_tls");
    println!(
        "cargo:rerun-if-changed={}",
        tls_dir.join("src/lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        tls_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        tls_dir.join("Cargo.lock").display()
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target = std::env::var("TARGET").unwrap();
    let target_dir = out.join("tls-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.arg("build")
        .arg("--release")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(tls_dir.join("Cargo.toml"))
        .arg("--target")
        .arg(&target)
        .arg("--target-dir")
        .arg(&target_dir);
    // Don't let the outer build's settings leak into the nested one.
    for v in [
        "CARGO_TARGET_DIR",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
        "MFLAGS",
    ] {
        cmd.env_remove(v);
    }
    let status = cmd.status().expect("could not run cargo to build kiln_tls");
    assert!(status.success(), "building kiln_tls failed");
    let name = if target.contains("msvc") {
        "kiln_tls.lib"
    } else {
        "libkiln_tls.a"
    };
    let built = target_dir.join(&target).join("release").join(name);
    std::fs::copy(&built, out.join(name))
        .unwrap_or_else(|e| panic!("could not copy {}: {e}", built.display()));
}
