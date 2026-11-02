//! `kiln self update` / `kiln self uninstall` against a scratch prefix and a
//! file:// release directory, and `kiln completions`.

use std::path::Path;
use std::process::Command;

fn kiln(args: &[&str], env: &[(&str, &Path)]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_kiln"));
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run kiln");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn sh(cmd: &str, dir: &Path) {
    let st = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "{cmd}");
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn self_update_and_uninstall() {
    let t = std::env::temp_dir().join(format!("kiln-self-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (prefix, rel, home) = (t.join("prefix"), t.join("rel"), t.join("home"));
    for d in [
        prefix.join("bin"),
        prefix.join("lib/kiln"),
        rel.clone(),
        home.clone(),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(prefix.join("bin/kiln"), "old").unwrap();
    std::fs::write(prefix.join("lib/kiln/old.txt"), "old").unwrap();
    let asset = format!("kiln-{}-linux.tar.gz", std::env::consts::ARCH);
    sh(
        &format!(
            "mkdir -p s/kiln-9.9.9/bin s/kiln-9.9.9/lib/kiln && \
             printf '#!/bin/sh\\necho kiln 9.9.9\\n' > s/kiln-9.9.9/bin/kiln && chmod +x s/kiln-9.9.9/bin/kiln && \
             echo new > s/kiln-9.9.9/lib/kiln/new.txt && tar -czf {asset} -C s kiln-9.9.9 && \
             sha256sum {asset} > {asset}.sha256"
        ),
        &rel,
    );
    let url = format!("file://{}", rel.display());
    let env: &[(&str, &Path)] = &[
        ("KILN_SELF_PREFIX", &prefix),
        ("KILN_DOWNLOAD_URL", Path::new(&url)),
        ("HOME", &home),
    ];

    let (code, out, _) = kiln(&["self", "update", "--dry-run"], env);
    assert_eq!(code, 0);
    assert!(
        out.contains(&format!("would download {url}/{asset}")),
        "{out}"
    );

    let (code, out, err) = kiln(&["self", "update"], env);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Installed kiln 9.9.9"), "{out}");
    assert!(prefix.join("lib/kiln/new.txt").is_file());
    assert!(!prefix.join("lib/kiln/old.txt").exists());
    assert!(!prefix.join("lib/kiln.old").exists());

    // A corrupted checksum is refused and leaves the install alone.
    std::fs::write(rel.join(format!("{asset}.sha256")), "0000  x\n").unwrap();
    let (code, _, err) = kiln(&["self", "update"], env);
    assert_eq!(code, 1);
    assert!(err.contains("checksum mismatch"), "{err}");
    assert!(prefix.join("lib/kiln/new.txt").is_file());

    std::fs::write(
        home.join(".bashrc"),
        "alias x=y\nexport PATH=\"$HOME/.kiln/bin:$PATH\" # added by the Kiln installer\n",
    )
    .unwrap();
    let (code, out, _) = kiln(&["self", "uninstall"], env);
    assert_eq!(code, 0);
    assert!(
        out.contains("would remove") && out.contains(".bashrc"),
        "{out}"
    );
    assert!(prefix.join("bin/kiln").exists());

    let (code, out, _) = kiln(&["self", "uninstall", "--yes"], env);
    assert_eq!(code, 0);
    assert!(out.contains("Kiln removed"), "{out}");
    assert!(!prefix.exists());
    assert_eq!(
        std::fs::read_to_string(home.join(".bashrc")).unwrap(),
        "alias x=y\n"
    );

    // Package-manager installs are not touched.
    let (code, _, err) = kiln(
        &["self", "uninstall", "--yes"],
        &[("KILN_SELF_PREFIX", Path::new("/usr"))],
    );
    assert_eq!(code, 1);
    assert!(err.contains("package manager"), "{err}");
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn completions_scripts() {
    for shell in ["bash", "zsh", "fish"] {
        let (code, out, _) = kiln(&["completions", shell], &[]);
        assert_eq!(code, 0);
        assert!(out.contains("build") && out.contains("release"), "{shell}");
    }
    let (code, _, _) = kiln(&["completions", "tcsh"], &[]);
    assert_eq!(code, 2);
    if Command::new("bash").arg("--version").output().is_ok() {
        let (_, script, _) = kiln(&["completions", "bash"], &[]);
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{script}\nCOMP_WORDS=(kiln se); COMP_CWORD=1; _kiln; echo \"${{COMPREPLY[@]}}\""
            ))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "self");
    }
}
