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
    /// `[link]`: native libraries and C sources for `extern "C"` code.
    pub link: NativeLink,
}

/// The `[link]` section: what to give the system linker.
///
/// ```toml
/// [link]
/// libs = ["z", "m"]                # -lz -lm
/// search = ["native/lib"]          # -L, relative to the package
/// c-sources = ["native/shim.c"]    # compiled with the system C compiler
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeLink {
    /// Library names (`-l`).
    pub libs: Vec<String>,
    /// Library search directories (`-L`), absolute.
    pub search: Vec<PathBuf>,
    /// C source files to compile and link, absolute.
    pub c_sources: Vec<PathBuf>,
}

impl NativeLink {
    /// Adds `other`'s entries that are not already present.
    pub fn merge(&mut self, other: &NativeLink) {
        for l in &other.libs {
            if !self.libs.contains(l) {
                self.libs.push(l.clone());
            }
        }
        for p in &other.search {
            if !self.search.contains(p) {
                self.search.push(p.clone());
            }
        }
        for p in &other.c_sources {
            if !self.c_sources.contains(p) {
                self.c_sources.push(p.clone());
            }
        }
    }
}

/// A one-line TOML array of strings: `["a", "b"]`.
fn string_array(v: &str) -> Option<Vec<String>> {
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    inner
        .split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(unquote)
        .collect()
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
        link: NativeLink::default(),
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
            if section != "package" && section != "dependencies" && section != "link" {
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
            "link" => {
                let xs = string_array(v).ok_or_else(|| {
                    err(&format!(
                        "`{k}` must be an array of strings, like `[\"z\"]`"
                    ))
                })?;
                match k {
                    "libs" => m.link.libs.extend(xs),
                    "search" => m.link.search.extend(xs.iter().map(|x| dir.join(x))),
                    "c-sources" => m.link.c_sources.extend(xs.iter().map(|x| dir.join(x))),
                    _ => {
                        return Err(err(&format!(
                            "unknown key `{k}` in [link] (expected libs, search, or c-sources)"
                        )))
                    }
                }
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

/// The `[link]` settings of `m` and all its dependencies.
pub fn all_link(m: &Manifest) -> Result<NativeLink, String> {
    let mut out = m.link.clone();
    for d in all_deps(m)? {
        let dir = d
            .root
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."));
        let dm = parse(&dir.join("kiln.toml"))?;
        out.merge(&dm.link);
    }
    Ok(out)
}

/// [`parse`], under the name the CLI uses.
pub fn parse_manifest(path: &Path) -> Result<Manifest, String> {
    parse(path)
}
