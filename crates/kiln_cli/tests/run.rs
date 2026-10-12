//! End-to-end tests: compile every `tests/run/*.kn` program with `kiln build`,
//! run it, and compare its output with the matching `.out` file.
//!
//! The expected file holds the program's stdout, then its stderr, then
//! `[exit N]` when the exit status is not 0. A first line `// flags: ...` passes
//! extra `kiln build` flags (e.g. `--release`). Set `KILN_BLESS=1` to rewrite the
//! expected files from the actual output.

use std::path::{Path, PathBuf};
use std::process::Command;

fn kiln() -> &'static str {
    env!("CARGO_BIN_EXE_kiln")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kiln-e2e-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds and runs `src`, returning the output in the `.out` format.
fn build_and_run(src: &Path, dir: &Path) -> Result<String, String> {
    let stem = src.file_stem().unwrap().to_str().unwrap();
    let exe = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
    // A first line `// flags: --release` passes build flags.
    let text = std::fs::read_to_string(src).unwrap();
    let flags: Vec<&str> = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("// flags:"))
        .map(|f| f.split_whitespace().collect())
        .unwrap_or_default();
    let b = Command::new(kiln())
        .arg("build")
        .args(&flags)
        .arg(src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    if !b.status.success() {
        return Err(format!(
            "`kiln build {}` failed:\n{}{}",
            src.display(),
            String::from_utf8_lossy(&b.stdout),
            String::from_utf8_lossy(&b.stderr)
        ));
    }
    let r = Command::new(&exe)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let mut out = String::from_utf8_lossy(&r.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&r.stderr));
    match r.status.code() {
        Some(0) => {}
        Some(c) => out.push_str(&format!("[exit {c}]\n")),
        None => out.push_str(&format!("[terminated: {}]\n", r.status)),
    }
    Ok(out.replace("\r\n", "\n"))
}

#[test]
fn run_programs() {
    let dir = root().join("tests/run");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "kn"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no programs in {}", dir.display());
    let tmp = scratch("run");
    let bless = std::env::var_os("KILN_BLESS").is_some();
    let mut failures = Vec::new();
    for f in &files {
        let expected_path = f.with_extension("out");
        match build_and_run(f, &tmp) {
            Err(e) => failures.push(e),
            Ok(actual) => {
                if bless {
                    std::fs::write(&expected_path, &actual).unwrap();
                    continue;
                }
                let expected = std::fs::read_to_string(&expected_path)
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                if actual.trim_end() != expected.trim_end() {
                    failures.push(format!(
                        "{}: output differs\n--- expected\n{expected}\n--- actual\n{actual}",
                        f.display()
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "{} of {} programs failed:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

#[test]
fn hello_example_runs() {
    let tmp = scratch("hello");
    let out = build_and_run(&root().join("examples/hello.kn"), &tmp).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(out, "Hello, Kiln!\n");
}

#[test]
fn kiln_run_forwards_exit_status() {
    let out = Command::new(kiln())
        .arg("run")
        .arg(root().join("tests/run/main_result.kn"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Error: 50 is too big"));
}

#[test]
fn unsupported_features_are_reported_not_miscompiled() {
    let dir = scratch("unsupported");
    let src = dir.join("slice.kn");
    std::fs::write(
        &src,
        "fun main() {\n\tv := [1, 2, 3]\n\tmatch &v[..] {\n\t\t[a, ..] => println(\"{a}\"),\n\t\t_ => {},\n\t}\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln())
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("slice"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("code generation does not support slice patterns"),
        "{err}"
    );
    assert!(err.contains("slice.kn:4:"), "{err}");
}

#[test]
fn borrow_errors_stop_the_build() {
    let dir = scratch("borrow");
    let src = dir.join("moved.kn");
    std::fs::write(
        &src,
        "fun main() {\n\ts := string::from(\"x\")\n\tt := s\n\tprintln(s)\n\tprintln(t)\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln())
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("moved"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("moved"));
}

/// Every program in `examples/` builds; `workers.kn` also runs to completion.
#[test]
fn examples_build() {
    let dir = scratch("examples");
    for entry in std::fs::read_dir(root().join("examples")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "kn") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        let exe = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
        let b = Command::new(kiln())
            .arg("build")
            .arg(&path)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            b.status.success(),
            "`kiln build {}` failed:\n{}",
            path.display(),
            String::from_utf8_lossy(&b.stderr)
        );
        if stem == "workers" {
            let r = Command::new(&exe).output().unwrap();
            let out = String::from_utf8_lossy(&r.stdout);
            assert!(r.status.success());
            assert_eq!(out.lines().count(), 10, "{out}");
            assert!(out.contains("finished job 9 (5 bytes)"), "{out}");
        }
    }
}

#[test]
fn stack_overflow_is_reported_clearly() {
    let dir = scratch("overflow");
    let src = dir.join("rec.kn");
    std::fs::write(
        &src,
        "fun rec(n i64) i64 {\n\tif n == 0 {\n\t\treturn 0\n\t}\n\treturn rec(n - 1) + 1\n}\n\nfun main() {\n\ta := [1 as u8; 1000000]\n\tprintln(\"{a[999999]}\")\n\t(ch, done) := chan::<i64>(0)\n\tgo move {\n\t\tch <- rec(1 << 40)\n\t}\n\tprintln(\"{<-done}\")\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln()).arg("run").arg(&src).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("fatal error: stack overflow"), "{err}");
    assert_ne!(out.status.code(), Some(0));

    // A frame too big for any goroutine stack is a compile-time error.
    std::fs::write(
        &src,
        "fun main() {\n\ta := [1 as u8; 50000000]\n\tprintln(\"{a[1]}\")\n}\n",
    )
    .unwrap();
    let out = Command::new(kiln())
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("big"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(err.contains("MiB of stack"), "{err}");
}

#[test]
fn fmt_check_and_write() {
    let dir = scratch("fmt");
    let src = dir.join("a.kn");
    std::fs::write(&src, "fun main(){\nx:=1+2\n    println(\"{x}\");\n}\n").unwrap();
    let out = Command::new(kiln())
        .arg("fmt")
        .arg("--check")
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("a.kn"));
    let out = Command::new(kiln()).arg("fmt").arg(&src).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&src).unwrap(),
        "fun main() {\n\tx := 1 + 2\n\tprintln(\"{x}\")\n}\n"
    );
    let out = Command::new(kiln())
        .arg("fmt")
        .arg("--check")
        .arg(&src)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    // Syntax errors: reported, file untouched.
    std::fs::write(&src, "fun main( {\n").unwrap();
    let out = Command::new(kiln()).arg("fmt").arg(&src).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax errors"));
}

/// Drives `kiln lsp` over stdio: initialize, open a file, hover, definition,
/// formatting, shutdown.
#[test]
fn lsp_session() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    let mut child = Command::new(kiln())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |v: String| {
        write!(stdin, "Content-Length: {}\r\n\r\n{v}", v.len()).unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || -> String {
        let mut len = 0;
        loop {
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                len = n.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        out.read_exact(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    };
    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#.into());
    assert!(recv().contains("hoverProvider"));
    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.into());
    let text = "fun add(a i32, b i32) i32 {\n\treturn a + b\n}\n\nfun main() {\n\tx := add(1, 2)\n\tprintln(\"{x}\")\n  y := undefined\n}\n";
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///t.kn","languageId":"kiln","version":1,"text":{}}}}}}}"#,
        json_str(text)
    );
    send(open);
    let diags = recv();
    assert!(
        diags.contains("publishDiagnostics") && diags.contains("undefined"),
        "{diags}"
    );
    send(r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":"file:///t.kn"},"position":{"line":5,"character":7}}}"#.into());
    let def = recv();
    assert!(
        def.contains(r#""start":{"character":3,"line":0}"#) || def.contains(r#""line":0"#),
        "{def}"
    );
    send(r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///t.kn"},"options":{"tabSize":4,"insertSpaces":false}}}"#.into());
    let f = recv();
    assert!(f.contains(r#"\ty := undefined"#), "{f}");
    send(r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#.into());
    recv();
    send(r#"{"jsonrpc":"2.0","method":"exit"}"#.into());
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// `kiln new`, modules in files, a path dependency, build caching, `kiln test`
/// on a package, and errors pointing into module files.
#[test]
fn packages_modules_and_dependencies() {
    let dir = scratch("pkg");
    let k = |args: &[&str], cwd: &std::path::Path| {
        Command::new(kiln())
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap()
    };
    assert!(k(&["new", "app"], &dir).status.success());
    assert!(k(&["new", "util", "--lib"], &dir).status.success());
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src/geo")).unwrap();
    std::fs::write(
        app.join("src/main.kn"),
        "mod geo\n\nimport geo::Point\nimport util::add\n\nfun main() {\n\tp := Point { x: 2, y: 3 }\n\tprintln(\"{add(p.x, p.y)} {geo::shapes::area(4)}\")\n}\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/geo/mod.kn"),
        "pub mod shapes\n\npub struct Point {\n\tpub x i64,\n\tpub y i64,\n}\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/geo/shapes.kn"),
        "pub fun area(s i64) i64 {\n\treturn s * s\n}\n\n@test\nfun squares() {\n\tassert_eq(area(3), 9)\n}\n",
    )
    .unwrap();
    let mut toml = std::fs::read_to_string(app.join("kiln.toml")).unwrap();
    toml.push_str("util = { path = \"../util\" }\n");
    std::fs::write(app.join("kiln.toml"), toml).unwrap();

    let out = k(&["run"], &app);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "5 16\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = k(&["build"], &app);
    assert!(String::from_utf8_lossy(&out.stdout).contains("up to date"));
    assert!(app
        .join("target/debug/app")
        .with_extension(std::env::consts::EXE_EXTENSION)
        .exists());
    let out = k(&["test"], &app);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("geo::shapes::squares ... ok") && s.contains("1 passed"),
        "{s}"
    );
    let out = k(&["test"], &dir.join("util"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));

    std::fs::write(
        app.join("src/geo/shapes.kn"),
        "pub fun area(s i64) i64 {\n\treturn s * true\n}\n",
    )
    .unwrap();
    let out = k(&["check"], &app);
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("shapes.kn:2:"), "{err}");
}
