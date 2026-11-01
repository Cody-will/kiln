//! `kiln-fuzz`: a dependency-free mutation fuzzer for the Kiln front ends.
//!
//! It starts from a corpus of `.kn` programs (by default every program under
//! `tests/` and `examples/`), applies random byte- and token-level mutations,
//! and feeds each result to a target:
//!
//! - `rust` (default): the in-tree Rust front end in-process: lexer, parser,
//!   resolver, type checker, and borrow checker, plus `kiln fmt`. Any panic is
//!   a crash.
//! - `kilnc`: the self-hosted compiler, `kilnc --check FILE`, one process per
//!   input. A crash is death by a signal, an exit status other than 0 or 1,
//!   a panic message, or a timeout.
//!
//! Crashing inputs are written to the output directory (default
//! `target/fuzz-crashes`). The run is bounded by `--iters` and `--secs`;
//! the same `--seed` reproduces the same inputs.
//!
//! ```text
//! kiln-fuzz [--target rust|kilnc] [--kilnc PATH] [--iters N] [--secs S]
//!           [--seed N] [--out DIR] [--replay FILE]... [CORPUS_DIR]...
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Tokens spliced into inputs, so mutations reach deeper than byte noise.
const DICT: &[&str] = &[
    "fun ",
    "struct ",
    "enum ",
    "trait ",
    "impl ",
    "for ",
    "if ",
    "else ",
    "match ",
    "return",
    "let ",
    "mut ",
    ":=",
    "=>",
    "<-",
    "::",
    "..",
    "..=",
    "&",
    "&mut ",
    "*",
    "?",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "<",
    ">",
    ",",
    ";",
    "\n",
    "'a",
    "dyn ",
    "Self",
    "self",
    "spawn ",
    "defer ",
    "select ",
    "chan::<i32>(1)",
    "unsafe ",
    "move ",
    "|x| x",
    "\"{x}\"",
    "r\"",
    "b\"\\x",
    "0xFF",
    "1e9f32",
    "3000000000",
    "'\\u{10FFFF}'",
    "/*",
    "*/",
    "@derive(Debug)",
    "where ",
    "impl Iterator<i32>",
    "fun(i32) i32",
    "option<",
    "result<",
    "Vec<",
    "Box<dyn ",
    "as ",
    "break",
    "continue",
    "loop ",
    "while ",
    "const ",
    "static ",
    "type ",
    "mod ",
    "import ",
    "pub ",
    "extern \"C\" ",
    "_",
    "i128",
    "u8",
    "!",
    "-",
    "+=",
    "<<",
    ">>=",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

fn mutate(rng: &mut Rng, input: &[u8], corpus: &[Vec<u8>]) -> Vec<u8> {
    let mut v = input.to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        let len = v.len();
        match rng.below(9) {
            0 if len > 0 => {
                let i = rng.below(len);
                v[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below(16.min(len - i));
                v.drain(i..i + n);
            }
            2 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below(64.min(len - i));
                let chunk = v[i..i + n].to_vec();
                let at = rng.below(v.len() + 1);
                v.splice(at..at, chunk);
            }
            3 | 4 => {
                let tok = DICT[rng.below(DICT.len())].as_bytes();
                let at = rng.below(len + 1);
                v.splice(at..at, tok.iter().copied());
            }
            5 if !corpus.is_empty() => {
                let other = &corpus[rng.below(corpus.len())];
                if !other.is_empty() {
                    let i = rng.below(other.len());
                    let n = 1 + rng.below(256.min(other.len() - i));
                    let at = rng.below(len + 1);
                    v.splice(at..at, other[i..i + n].iter().copied());
                }
            }
            6 if len > 0 => v.truncate(rng.below(len)),
            7 => {
                // Deep nesting: stresses recursion in every pass.
                const PAIRS: &[(&str, &str)] = &[
                    ("(", ")"),
                    ("[", "]"),
                    ("{\n", "}\n"),
                    ("-", ""),
                    ("Box<", ">"),
                    ("Some(", ")"),
                    ("1 + ", ""),
                    ("/*", "*/"),
                ];
                let (open, close) = PAIRS[rng.below(PAIRS.len())];
                let n = 1 + rng.below(2000);
                let at = rng.below(len + 1);
                let mut mid = open.repeat(n).into_bytes();
                mid.extend_from_slice(b"x");
                mid.extend_from_slice(close.repeat(n).as_bytes());
                v.splice(at..at, mid);
            }
            _ if len > 0 => {
                let i = rng.below(len);
                const BYTES: &[u8] = b"(){}[]<>;,.:'\"\\\n\t &|*?!=_0aZ";
                v[i] = BYTES[rng.below(BYTES.len())];
            }
            _ => {}
        }
    }
    v
}

/// Runs the Rust front end and formatter; `Err` carries the panic message.
fn run_rust(src: &str) -> Result<(), String> {
    let r = std::panic::catch_unwind(|| {
        let _ = kiln_lexer::lex(src);
        let parsed = kiln_parser::parse(src);
        if parsed.errors.is_empty() {
            let _ = kiln_fmt::format(src);
        }
        let _ = kiln_borrowck::check_parse(&parsed);
    });
    r.map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "panic".to_string())
    })
}

fn run_kilnc(kilnc: &Path, file: &Path, timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(kilnc)
        .arg("--check")
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", kilnc.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut err);
                }
                if err.contains("panic") || err.contains("Segmentation") {
                    return Err(format!("{status}: {}", err.trim()));
                }
                return match status.code() {
                    Some(0) | Some(1) => Ok(()),
                    _ => Err(format!("{status}: {}", err.trim())),
                };
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timeout after {timeout:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "kn") {
            out.push(p);
        }
    }
}

const USAGE: &str = "usage: kiln-fuzz [--target rust|kilnc] [--kilnc PATH] [--iters N] [--secs S] [--seed N] [--out DIR] [--replay FILE]... [CORPUS_DIR]...";

fn main() -> ExitCode {
    let mut target = "rust".to_string();
    let mut kilnc = PathBuf::from("compiler/target/release/kilnc");
    let mut iters: u64 = 10_000;
    let mut secs: u64 = 60;
    let mut seed: u64 = 0x5eed;
    let mut out = PathBuf::from("target/fuzz-crashes");
    let mut dirs = Vec::new();
    let mut replay = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_default();
        match a.as_str() {
            "--target" => target = val(),
            "--kilnc" => kilnc = PathBuf::from(val()),
            "--iters" => iters = val().parse().unwrap_or(iters),
            "--secs" => secs = val().parse().unwrap_or(secs),
            "--seed" => seed = val().parse().unwrap_or(seed),
            "--out" => out = PathBuf::from(val()),
            "--replay" => replay.push(PathBuf::from(val())),
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ if a.starts_with('-') => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            _ => dirs.push(PathBuf::from(a)),
        }
    }
    if target != "rust" && target != "kilnc" {
        eprintln!("unknown target `{target}`\n{USAGE}");
        return ExitCode::from(2);
    }
    // Panics are expected findings; keep the output to one line each.
    // `KILN_FUZZ_VERBOSE=1` keeps the default hook (backtraces).
    if std::env::var_os("KILN_FUZZ_VERBOSE").is_none() {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let tmp = std::env::temp_dir().join(format!("kiln-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let input_path = tmp.join("input.kn");
    let timeout = Duration::from_secs(10);
    let run = |data: &[u8]| -> Result<(), String> {
        let text = String::from_utf8_lossy(data);
        if target == "rust" {
            run_rust(&text)
        } else {
            std::fs::write(&input_path, text.as_bytes()).unwrap();
            run_kilnc(&kilnc, &input_path, timeout)
        }
    };

    if !replay.is_empty() {
        let mut bad = 0;
        for f in &replay {
            let data = std::fs::read(f).unwrap_or_default();
            match run(&data) {
                Ok(()) => println!("ok    {}", f.display()),
                Err(e) => {
                    bad += 1;
                    println!("CRASH {}: {e}", f.display());
                }
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return if bad == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    if dirs.is_empty() {
        dirs = vec![PathBuf::from("tests"), PathBuf::from("examples")];
    }
    let mut files = Vec::new();
    for d in &dirs {
        collect(d, &mut files);
    }
    let corpus: Vec<Vec<u8>> = files.iter().filter_map(|f| std::fs::read(f).ok()).collect();
    if corpus.is_empty() {
        eprintln!("kiln-fuzz: empty corpus");
        return ExitCode::from(2);
    }
    let mut rng = Rng(seed | 1);
    let start = Instant::now();
    let mut crashes = std::collections::BTreeMap::<String, PathBuf>::new();
    let mut n = 0;
    while n < iters && start.elapsed() < Duration::from_secs(secs) {
        n += 1;
        let base = &corpus[rng.below(corpus.len())];
        let input = mutate(&mut rng, base, &corpus);
        if let Err(msg) = run(&input) {
            // One saved input per distinct message.
            let key: String = msg.lines().next().unwrap_or("").chars().take(160).collect();
            if !crashes.contains_key(&key) {
                std::fs::create_dir_all(&out).unwrap();
                let p = out.join(format!("crash-{}.kn", crashes.len()));
                std::fs::write(&p, &input).unwrap();
                println!("CRASH {}: {key}", p.display());
                crashes.insert(key, p);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!(
        "kiln-fuzz: target={target} seed={seed} inputs={n} corpus={} distinct_crashes={} in {:.1}s",
        corpus.len(),
        crashes.len(),
        start.elapsed().as_secs_f64()
    );
    if crashes.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
