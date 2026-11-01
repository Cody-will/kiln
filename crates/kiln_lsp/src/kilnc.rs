//! Running the self-hosted front end: `kilnc --check` (parse, resolve, type
//! check, borrow check). `kiln check` and the language server's diagnostics
//! use it when kilnc is installed; the Rust front end is only the fallback
//! for a `kiln` without kilnc (bootstrap and the differential tests).

use kiln_lexer::{Diagnostic, Span};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where kilnc is and the directory with its inputs (`KILNC_RESOLVE_DIR`).
#[derive(Debug, Clone)]
pub struct Kilnc {
    /// The kilnc executable.
    pub exe: PathBuf,
    /// Std sources and runtime libraries kilnc reads.
    pub resolve_dir: PathBuf,
}

/// The result of `kilnc --check`.
#[derive(Debug, Default)]
pub struct Checked {
    /// Each loaded file's span base and path.
    pub files: Vec<(usize, PathBuf)>,
    /// Diagnostics of the first failing phase, with global spans.
    pub diagnostics: Vec<Diagnostic>,
    /// An error without a location (a missing module file, a bad manifest).
    pub fatal: Option<String>,
    /// Navigation facts (`kilnc --index` only; see [`crate::facts`]), one per line.
    pub facts: String,
}

impl Checked {
    /// No errors.
    pub fn ok(&self) -> bool {
        self.diagnostics.is_empty() && self.fatal.is_none()
    }
}

fn unfield(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some(x) => o.push(x),
            None => o.push('\\'),
        }
    }
    o
}

/// Parses `kilnc --check` output.
pub fn parse_output(out: &str) -> Checked {
    let mut c = Checked::default();
    for line in out.lines() {
        if let Some(fact) = line.strip_prefix("fact\t") {
            c.facts.push_str(fact);
            c.facts.push('\n');
            continue;
        }
        let f: Vec<&str> = line.splitn(4, '\t').collect();
        let span = |a: &str, b: &str| -> Option<Span> {
            Some(Span::new(a.parse().ok()?, b.parse().ok()?))
        };
        match f.as_slice() {
            ["file", base, path] => {
                if let Ok(b) = base.parse() {
                    c.files.push((b, PathBuf::from(unfield(path))));
                }
            }
            ["error", a, b, m] => c.diagnostics.push(Diagnostic::error(
                unfield(m),
                span(a, b).unwrap_or(Span::new(0, 0)),
            )),
            ["note", a, b, m] => {
                if let Some(d) = c.diagnostics.last_mut() {
                    d.notes.push((unfield(m), span(a, b)));
                }
            }
            ["help", m] => {
                if let Some(d) = c.diagnostics.last_mut() {
                    d.help = Some(unfield(m));
                }
            }
            ["fatal", m] => c.fatal = Some(unfield(m)),
            _ => {}
        }
    }
    c
}

/// Runs `kilnc --check` on `root`, reading `overlays` (path, text) instead
/// of the files on disk.
pub fn check(k: &Kilnc, root: &Path, overlays: &[(PathBuf, String)]) -> Result<Checked, String> {
    run(k, "--check", root, overlays, &|| false)
}

/// [`check`] plus the navigation facts (`kilnc --index`).
pub fn index(k: &Kilnc, root: &Path, overlays: &[(PathBuf, String)]) -> Result<Checked, String> {
    run(k, "--index", root, overlays, &|| false)
}

/// The error of a run stopped by its `cancel` callback.
pub const CANCELLED: &str = "kilnc: cancelled";

/// [`index`], killing kilnc (and returning `Err(CANCELLED)`) as soon as
/// `cancel` returns true (newer edits made this run stale).
pub fn index_cancellable(
    k: &Kilnc,
    root: &Path,
    overlays: &[(PathBuf, String)],
    cancel: &dyn Fn() -> bool,
) -> Result<Checked, String> {
    run(k, "--index", root, overlays, cancel)
}

fn run(
    k: &Kilnc,
    mode: &str,
    root: &Path,
    overlays: &[(PathBuf, String)],
    cancel: &dyn Fn() -> bool,
) -> Result<Checked, String> {
    let mut cmd = Command::new(&k.exe);
    cmd.arg(mode);
    let mut tmp = None;
    if !overlays.is_empty() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kiln-overlay-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        for (i, (p, text)) in overlays.iter().enumerate() {
            let f = dir.join(format!("{i}.kn"));
            std::fs::write(&f, text).map_err(|e| format!("cannot write {}: {e}", f.display()))?;
            cmd.arg("--overlay").arg(p).arg(&f);
        }
        tmp = Some(dir);
    }
    cmd.arg(root);
    cmd.env("KILNC_RESOLVE_DIR", &k.resolve_dir);
    let o = wait(cmd, cancel);
    if let Some(d) = tmp {
        let _ = std::fs::remove_dir_all(d);
    }
    let o = o?.ok_or_else(|| CANCELLED.to_string())?;
    let stdout = String::from_utf8_lossy(&o.stdout);
    let c = parse_output(&stdout);
    // A crash (no verdict printed) is an error, not a clean program.
    if !o.status.success() && c.ok() {
        return Err(format!(
            "kilnc {mode} failed: {}{}",
            String::from_utf8_lossy(&o.stderr),
            stdout
        ));
    }
    Ok(c)
}

/// Runs `cmd` to completion (`Ok(None)` if `cancel` fired and it was killed).
fn wait(
    mut cmd: Command,
    cancel: &dyn Fn() -> bool,
) -> Result<Option<std::process::Output>, String> {
    use std::io::Read;
    use std::process::Stdio;
    let exe = cmd.get_program().to_string_lossy().to_string();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {exe}: {e}"))?;
    // Drain both pipes on threads so a large output cannot block kilnc.
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut v);
            }
            v
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(st);
        }
        if cancel() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let (stdout, stderr) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    Ok(status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_output() {
        let c = parse_output(
            "file\t0\ta.kn\nerror\t52\t57\tuse of moved value: `x`\nnote\t41\t42\tvalue moved here\nnote\t-\t-\tplain\\tnote\nhelp\tclone it\nfact\tbind\t1\t2\t1\t2\n",
        );
        assert_eq!(c.files, vec![(0, PathBuf::from("a.kn"))]);
        assert_eq!(c.diagnostics.len(), 1);
        let d = &c.diagnostics[0];
        assert_eq!(d.span, Span::new(52, 57));
        assert_eq!(
            d.notes[0],
            ("value moved here".into(), Some(Span::new(41, 42)))
        );
        assert_eq!(d.notes[1], ("plain\tnote".into(), None));
        assert_eq!(d.help.as_deref(), Some("clone it"));
        assert!(!c.ok());
        assert_eq!(c.facts, "bind\t1\t2\t1\t2\n");
    }

    #[cfg(unix)]
    #[test]
    fn cancel_kills_the_child() {
        let t = std::time::Instant::now();
        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        let r = wait(cmd, &|| t.elapsed() > std::time::Duration::from_millis(50)).unwrap();
        assert!(r.is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        let r = wait(Command::new("true"), &|| false).unwrap().unwrap();
        assert!(r.status.success());
    }
}
