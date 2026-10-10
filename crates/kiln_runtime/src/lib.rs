//! The Kiln runtime, as a static library embedded in the compiler.
//!
//! `runtime/kiln_rt.c` is compiled by `build.rs` when the `kiln` tool is built.
//! [`LIB`] holds the archive's bytes; `kiln build` writes it next to the object
//! file it generates and passes both to the system linker, so users don't need
//! a C compiler (only a linker).
//!
//! The functions it exports (`kn_alloc`, `kn_panic`, `kn_print`,
//! `kn_string_push`, `kn_fmt_f64`, ...) are declared by `kiln_codegen`.

/// The runtime static library.
#[cfg(target_env = "msvc")]
pub static LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kiln_rt.lib"));
/// The runtime static library.
#[cfg(not(target_env = "msvc"))]
pub static LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libkiln_rt.a"));

/// The library's file name for this target.
#[cfg(target_env = "msvc")]
pub const LIB_NAME: &str = "kiln_rt.lib";
/// The library's file name for this target.
#[cfg(not(target_env = "msvc"))]
pub const LIB_NAME: &str = "libkiln_rt.a";

/// The TLS static library (rustls behind a C ABI; see `crates/kiln_tls`).
#[cfg(target_env = "msvc")]
pub static TLS_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kiln_tls.lib"));
/// The TLS static library (rustls behind a C ABI; see `crates/kiln_tls`).
#[cfg(not(target_env = "msvc"))]
pub static TLS_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libkiln_tls.a"));

/// The TLS library's file name for this target.
#[cfg(target_env = "msvc")]
pub const TLS_LIB_NAME: &str = "kiln_tls.lib";
/// The TLS library's file name for this target.
#[cfg(not(target_env = "msvc"))]
pub const TLS_LIB_NAME: &str = "libkiln_tls.a";

#[cfg(test)]
mod tests {
    #[test]
    fn library_is_embedded() {
        assert!(super::LIB.len() > 100);
    }
}
