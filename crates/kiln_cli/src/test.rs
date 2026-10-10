//! `kiln test`: find `#[test]` functions, build one test executable per
//! file, and run each test in its own process (in parallel), so a panic
//! fails only that test.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Settings for one `kiln test` run.
pub struct TestOpts {
    pub codegen: kiln_codegen::Options,
    /// Only tests whose name contains this.
    pub filter: Option<String>,
    /// Run `#[ignore]` tests (only them, as `cargo test -- --ignored` does).
    pub ignored: bool,
    /// Per-test time limit.
    pub timeout: Duration,
    /// Files and directories to search; empty means the current directory.
    pub paths: Vec<String>,
}

/// Parses `kiln test` arguments.
pub fn parse_args(rest: &[String]) -> Option<TestOpts> {
    let mut o = TestOpts {
        codegen: kiln_codegen::Options::default(),
        filter: None,
        ignored: false,
        timeout: Duration::from_secs(600),
        paths: vec![],
    };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--release" => o.codegen.release = true,
            "--ignored" => o.ignored = true,
            "--run" => o.filter = Some(it.next()?.clone()),
            "--timeout" => o.timeout = Duration::from_secs_f64(it.next()?.parse().ok()?),
            _ if a.starts_with('-') => return None,
            _ => o.paths.push(a.clone()),
        }
    }
    Some(o)
}

/// Files under `paths` that contain tests: every `.kn` file named
/// explicitly, plus `.kn` files containing `#[test]` found recursively in
/// directories (skipping hidden directories and `target`).
fn discover(paths: &[String]) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    };
    let mut out = vec![];
    for r in roots {
        if r.is_dir() {
            walk(&r, &mut out);
        } else {
            out.push(r);
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !name.starts_with('.') && name != "target" {
                walk(&p, out);
            }
        } else if name.ends_with(".kn")
            && std::fs::read_to_string(&p).is_ok_and(|s| s.contains("#[test]"))
        {
            out.push(p);
        }
    }
}

enum Outcome {
    Pass,
    Fail(String),
    Ignored,
}

/// Runs the tests; exit status 0 when all pass, 1 otherwise, 2 on usage errors.
pub fn run(o: TestOpts) -> ExitCode {
    let files = discover(&o.paths);
    if files.is_empty() {
        println!("no test files found");
        return ExitCode::SUCCESS;
    }
    let start = Instant::now();
    let (mut passed, mut failed, mut ignored, mut filtered) = (0, 0, 0, 0);
    let mut failures: Vec<String> = vec![];
    let dir = std::env::temp_dir().join(format!("kiln-test-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return ExitCode::from(1);
    }
    for (fi, file) in files.iter().enumerate() {
        let shown = file.display().to_string();
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read `{shown}`: {e}");
                failed += 1;
                failures.push(shown);
                continue;
            }
        };
        let exe = dir.join(format!("t{fi}{}", std::env::consts::EXE_SUFFIX));
        let tests = match kiln_codegen::build_tests(&src, &exe, o.codegen) {
            Ok(t) => t,
            Err(kiln_codegen::BuildError::Diagnostics(errs)) => {
                crate::report(&shown, &src, &errs);
                failed += 1;
                failures.push(format!("{shown} (does not compile)"));
                continue;
            }
            Err(kiln_codegen::BuildError::Other(msg)) => {
                eprintln!("error: {msg}");
                failed += 1;
                failures.push(format!("{shown} (does not build)"));
                continue;
            }
        };
        let selected: Vec<_> = tests
            .iter()
            .filter(|t| {
                o.filter
                    .as_ref()
                    .is_none_or(|f| t.name.contains(f.as_str()))
            })
            .collect();
        filtered += tests.len() - selected.len();
        if selected.is_empty() {
            continue;
        }
        println!(
            "\nrunning {} test{} in {shown}",
            selected.len(),
            if selected.len() == 1 { "" } else { "s" }
        );
        let results = run_all(&exe, &selected, &o);
        for (t, r) in selected.iter().zip(results) {
            match r {
                Outcome::Pass => {
                    passed += 1;
                    println!("test {} ... ok", t.name);
                }
                Outcome::Ignored => {
                    ignored += 1;
                    println!("test {} ... ignored", t.name);
                }
                Outcome::Fail(out) => {
                    failed += 1;
                    println!("test {} ... FAILED", t.name);
                    println!("---- {} output ----\n{}", t.name, out.trim_end());
                    failures.push(format!("{shown}: {}", t.name));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if !failures.is_empty() {
        println!("\nfailures:");
        for f in &failures {
            println!("    {f}");
        }
    }
    println!(
        "\ntest result: {}. {passed} passed; {failed} failed; {ignored} ignored; {filtered} filtered out; finished in {:.2}s",
        if failed == 0 { "ok" } else { "FAILED" },
        start.elapsed().as_secs_f64()
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_all(exe: &Path, tests: &[&kiln_parser::TestCase], o: &TestOpts) -> Vec<Outcome> {
    let n = std::thread::available_parallelism().map_or(4, |n| n.get());
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<Outcome>> = (0..tests.len()).map(|_| None).collect();
    let slots: Vec<std::sync::Mutex<&mut Option<Outcome>>> =
        results.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|s| {
        for _ in 0..n.min(tests.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= tests.len() {
                    break;
                }
                let r = run_one(exe, tests[i], o);
                **slots[i].lock().unwrap() = Some(r);
            });
        }
    });
    drop(slots);
    results.into_iter().map(|r| r.unwrap()).collect()
}

fn run_one(exe: &Path, t: &kiln_parser::TestCase, o: &TestOpts) -> Outcome {
    if t.ignore != o.ignored {
        return Outcome::Ignored;
    }
    let child = Command::new(exe)
        .arg(&t.name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Outcome::Fail(format!("cannot run test executable: {e}")),
    };
    // Drain the pipes on threads so a chatty test cannot block.
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ho = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut so, &mut v);
        v
    });
    let he = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut se, &mut v);
        v
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > o.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let mut out = String::from_utf8_lossy(&ho.join().unwrap_or_default()).into_owned();
    out.push_str(&String::from_utf8_lossy(&he.join().unwrap_or_default()));
    let Some(status) = status else {
        out.push_str(&format!("test timed out after {:?}\n", o.timeout));
        return Outcome::Fail(out);
    };
    let panicked = status.code() == Some(101);
    match &t.should_panic {
        None if status.success() => Outcome::Pass,
        None => {
            if status.code().is_none() {
                out.push_str(&format!("test terminated abnormally ({status})\n"));
            }
            Outcome::Fail(out)
        }
        Some(_) if !panicked => {
            out.push_str("note: test did not panic as expected\n");
            Outcome::Fail(out)
        }
        Some(Some(want)) if !out.contains(want.as_str()) => {
            out.push_str(&format!(
                "note: panic did not contain expected string\n      expected substring: {want:?}\n"
            ));
            Outcome::Fail(out)
        }
        Some(_) => Outcome::Pass,
    }
}
