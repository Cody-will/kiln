//! `kiln.toml`: the package manifest, and resolving path dependencies.

use crate::package::Dependency;
use std::path::{Path, PathBuf};

/// A parsed `kiln.toml`.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The package name.
    pub name: String,
    /// The package version.
    pub version: String,
    /// The directory holding `kiln.toml`.
    pub dir: PathBuf,
    /// Path dependencies: (name, directory).
    pub deps: Vec<(String, PathBuf)>,
}

/// Finds `kiln.toml` in `start` or a parent directory.
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    let mut d: Option<&Path> = Some(&start);
    while let Some(dir) = d {
        let m = dir.join("kiln.toml");
        if m.is_file() {
            return Some(m);
        }
        d = dir.parent();
    }
    None
}

fn unquote(v: &str) -> Option<String> {
    let v = v.trim();
    let v = v.strip_prefix('"')?.strip_suffix('"')?;
    Some(v.replace("\\\\", "\\").replace("\\\"", "\""))
}

/// Parses the subset of TOML a manifest uses: `[package]` with `name` and
/// `version`, and `[dependencies]` with `name = { path = "..." }`.
pub fn parse(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut m = Manifest {
        name: String::new(),
        version: "0.0.0".into(),
        dir: dir.clone(),
        deps: vec![],
    };
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = match raw.find('#') {
            Some(i) if !raw[..i].contains('"') => &raw[..i],
            _ => raw,
        }
        .trim();
        let err = |msg: &str| format!("{}:{}: {msg}", path.display(), n + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[') {
            section = s
                .strip_suffix(']')
                .ok_or_else(|| err("expected `]`"))?
                .trim()
                .to_string();
            if section != "package" && section != "dependencies" {
                return Err(err(&format!("unknown section `[{section}]`")));
            }
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| err("expected `key = value`"))?;
        let (k, v) = (k.trim(), v.trim());
        match section.as_str() {
            "package" => match k {
                "name" => m.name = unquote(v).ok_or_else(|| err("`name` must be a string"))?,
                "version" => {
                    m.version = unquote(v).ok_or_else(|| err("`version` must be a string"))?
                }
                "edition" | "description" | "license" | "authors" => {}
                _ => return Err(err(&format!("unknown key `{k}` in [package]"))),
            },
            "dependencies" => {
                let inner = v
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                    .ok_or_else(|| err("a dependency must be `{ path = \"...\" }` (registry dependencies are not supported yet)"))?;
                let mut p = None;
                for kv in inner.split(',') {
                    if let Some((a, b)) = kv.split_once('=') {
                        if a.trim() == "path" {
                            p = unquote(b);
                        }
                    }
                }
                let p = p.ok_or_else(|| err("a dependency needs `path = \"...\"`"))?;
                if !is_ident(k) {
                    return Err(err(&format!("dependency name `{k}` must be an identifier")));
                }
                m.deps.push((k.to_string(), dir.join(p)));
            }
            _ => return Err(err("key outside a section")),
        }
    }
    if m.name.is_empty() {
        return Err(format!("{}: [package] needs a `name`", path.display()));
    }
    Ok(m)
}

/// Whether `s` is a valid identifier (package and dependency names).
pub fn is_ident(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// All dependencies of `m`, transitively (each library once), in order.
pub fn all_deps(m: &Manifest) -> Result<Vec<Dependency>, String> {
    let mut out: Vec<Dependency> = Vec::new();
    let mut stack = vec![m.clone()];
    let mut seen = vec![m.dir.canonicalize().unwrap_or(m.dir.clone())];
    while let Some(cur) = stack.pop() {
        for (name, dir) in &cur.deps {
            let canon = dir
                .canonicalize()
                .map_err(|_| format!("dependency `{name}`: no package at {}", dir.display()))?;
            if seen.contains(&canon) {
                continue;
            }
            seen.push(canon);
            let dm =
                parse(&dir.join("kiln.toml")).map_err(|e| format!("dependency `{name}`: {e}"))?;
            let root = dir.join("src").join("lib.kn");
            if !root.is_file() {
                return Err(format!(
                    "dependency `{name}` is not a library: {} is missing",
                    root.display()
                ));
            }
            if out.iter().any(|d| d.name == *name) {
                return Err(format!("two dependencies are named `{name}`"));
            }
            out.push(Dependency {
                name: name.clone(),
                root,
            });
            stack.push(dm);
        }
    }
    Ok(out)
}

/// [`parse`], under the name the CLI uses.
pub fn parse_manifest(path: &Path) -> Result<Manifest, String> {
    parse(path)
}
