//! Compiles the C runtime into a static library that `kiln build` links into
//! every executable.

fn main() {
    println!("cargo:rerun-if-changed=runtime/kiln_rt.c");
    println!("cargo:rerun-if-changed=runtime/kiln_sched.c");
    println!("cargo:rerun-if-changed=runtime/kiln_std.c");
    cc::Build::new()
        .file("runtime/kiln_rt.c")
        .file("runtime/kiln_sched.c")
        .file("runtime/kiln_std.c")
        .opt_level(2)
        .warnings(true)
        .cargo_metadata(false)
        .compile("kiln_rt");
}
