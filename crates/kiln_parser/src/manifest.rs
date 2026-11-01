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
    /// Registry dependencies: (name, version requirement), in order.
    pub reg: Vec<(String, String)>,
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
        reg: vec![],
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
                if !is_ident(k) {
                    return Err(err(&format!("dependency name `{k}` must be an identifier")));
                }
                // `name = "1.2"`: a registry dependency.
                if let Some(req) = unquote(v) {
                    m.reg.push((k.to_string(), req));
                    continue;
                }
                let inner = v
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                    .ok_or_else(|| {
                        err("a dependency must be `\"<version>\"`, `{ version = \"...\" }`, or `{ path = \"...\" }`")
                    })?;
                let mut p = None;
                let mut ver = None;
                for kv in inner.split(',') {
                    if let Some((a, b)) = kv.split_once('=') {
                        match a.trim() {
                            "path" => p = unquote(b),
                            "version" => ver = unquote(b),
                            _ => {}
                        }
                    }
                }
                match (p, ver) {
                    (Some(p), _) => m.deps.push((k.to_string(), dir.join(p))),
                    (None, Some(req)) => m.reg.push((k.to_string(), req)),
                    (None, None) => {
                        return Err(err(
                            "a dependency needs `path = \"...\"` or `version = \"...\"`",
                        ))
                    }
                }
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

/// Where kiln keeps downloaded packages and registry indexes:
/// `$KILN_HOME`, or `~/.kiln`.
pub fn kiln_home() -> PathBuf {
    if let Some(h) = std::env::var_os("KILN_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".kiln")
}

/// The unpacked source of registry package `name` at `version`.
pub fn registry_src(name: &str, version: &str) -> PathBuf {
    kiln_home()
        .join("registry")
        .join("src")
        .join(format!("{name}-{version}"))
}

static RESOLVED: std::sync::Mutex<Option<Vec<(String, String)>>> = std::sync::Mutex::new(None);

/// Overrides the lock file: the registry versions just resolved (the CLI
/// sets these before it writes `kiln.lock`).
pub fn set_registry_versions(v: Vec<(String, String)>) {
    *RESOLVED.lock().unwrap() = Some(v);
}

/// The versions of the registry packages pinned in `dir/kiln.lock` (or set
/// with [`set_registry_versions`]).
pub fn locked_registry_versions(dir: &Path) -> Vec<(String, String)> {
    if let Some(v) = RESOLVED.lock().unwrap().clone() {
        return v;
    }
    let Ok(text) = std::fs::read_to_string(dir.join("kiln.lock")) else {
        return vec![];
    };
    let mut out = vec![];
    let (mut name, mut version) = (None, None);
    for line in text.lines().chain(std::iter::once("[[package]]")) {
        let line = line.trim();
        if line == "[[package]]" {
            if let (Some(n), Some(v)) = (name.take(), version.take()) {
                out.push((n, v));
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "name" => name = unquote(v),
                "version" => version = unquote(v),
                "source" if !unquote(v).is_some_and(|s| s.starts_with("registry+")) => {
                    name = None;
                }
                _ => {}
            }
        }
    }
    out
}

/// All dependencies of `m`, transitively (each library once), in order.
/// Registry dependencies use the versions pinned in `m`'s `kiln.lock`.
pub fn all_deps(m: &Manifest) -> Result<Vec<Dependency>, String> {
    let locked = locked_registry_versions(&m.dir);
    let mut out: Vec<Dependency> = Vec::new();
    let mut stack = vec![m.clone()];
    let mut seen = vec![m.dir.canonicalize().unwrap_or(m.dir.clone())];
    let mut first = true;
    while let Some(cur) = stack.pop() {
        let direct = std::mem::take(&mut first);
        let mut deps = cur.deps.clone();
        for (name, _) in &cur.reg {
            let Some((_, v)) = locked.iter().find(|(n, _)| n == name) else {
                return Err(format!(
                    "registry dependency `{name}` is not in kiln.lock; run `kiln update` to resolve it"
                ));
            };
            deps.push((name.clone(), registry_src(name, v)));
        }
        for (name, dir) in &deps {
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
                uses: dm
                    .deps
                    .iter()
                    .map(|(n, _)| n.clone())
                    .chain(dm.reg.iter().map(|(n, _)| n.clone()))
                    .collect(),
                direct,
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
