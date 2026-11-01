//! End-to-end registry workflow against a local file index.

use std::fs;
use std::path::Path;
use std::process::Command;

fn kiln(dir: &Path, home: &Path, index: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(dir)
        .env("KILN_HOME", home)
        .env("KILN_REGISTRY", index)
        .env("KILN_COMPILER", "rust")
        .env_remove("KILNC")
        .output()
        .unwrap();
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.success(), s)
}

fn pkg(root: &Path, name: &str, ver: &str, deps: &str, lib: &str) -> std::path::PathBuf {
    let d = root.join(format!("{name}-{ver}"));
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(
        d.join("kiln.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"{ver}\"\n\n[dependencies]\n{deps}"),
    )
    .unwrap();
    fs::write(d.join("src/lib.kn"), lib).unwrap();
    d
}

#[test]
fn publish_add_lock_offline() {
    let t = std::env::temp_dir().join(format!("kiln-reg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&t);
    fs::create_dir_all(&t).unwrap();
    let (home, index) = (t.join("home"), t.join("index"));
    let (ok, out) = kiln(
        &t,
        &home,
        &index,
        &["publish", "--init-index", index.to_str().unwrap()],
    );
    assert!(ok, "{out}");
    for (v, body) in [("0.1.0", "old"), ("0.2.0", "hello")] {
        let d = pkg(
            &t,
            "greet",
            v,
            "",
            &format!("pub fun hi(n &str) string {{\n\treturn \"{body} {{n}}\"\n}}\n"),
        );
        let (ok, out) = kiln(&d, &home, &index, &["publish"]);
        assert!(ok, "{out}");
    }
    let d = pkg(
        &t,
        "shout",
        "0.1.0",
        "greet = \"0.2\"\n",
        "pub fun shout(n &str) string {\n\treturn greet::hi(n).to_upper()\n}\n",
    );
    let (ok, out) = kiln(&d, &home, &index, &["publish"]);
    assert!(ok, "{out}");

    let app = t.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("kiln.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main.kn"),
        "fun main() {\n\tprintln(shout::shout(\"bob\"))\n\tprintln(greet::hi(\"x\"))\n}\n",
    )
    .unwrap();
    // Conflict: greet 0.1 vs shout's greet 0.2 is rejected and kiln.toml rolled back.
    let (ok, _) = kiln(&app, &home, &index, &["add", "greet@0.1"]);
    assert!(ok);
    let (ok, out) = kiln(&app, &home, &index, &["add", "shout"]);
    assert!(!ok, "{out}");
    assert!(!fs::read_to_string(app.join("kiln.toml"))
        .unwrap()
        .contains("shout"));
    let toml = fs::read_to_string(app.join("kiln.toml")).unwrap();
    fs::write(app.join("kiln.toml"), toml.replace("\"0.1\"", "\"0.2\"")).unwrap();
    let (ok, out) = kiln(&app, &home, &index, &["update"]);
    assert!(ok, "{out}");
    let (ok, out) = kiln(&app, &home, &index, &["add", "shout"]);
    assert!(ok, "{out}");
    let (ok, out) = kiln(&app, &home, &index, &["run"]);
    assert!(
        ok && out.contains("HELLO BOB") && out.contains("hello x"),
        "{out}"
    );
    let lock = fs::read_to_string(app.join("kiln.lock")).unwrap();
    assert!(
        lock.contains("version = \"0.2.0\"") && lock.contains("checksum = \"sha256:"),
        "{lock}"
    );
    // Offline + locked works from the cache, even with the index gone.
    fs::rename(&index, t.join("gone")).unwrap();
    let (ok, out) = kiln(&app, &home, &index, &["build", "--locked", "--offline"]);
    assert!(ok, "{out}");
    let _ = fs::remove_dir_all(&t);
}
