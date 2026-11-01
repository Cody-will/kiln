//! Registry dependencies: a git (or local directory) index, semver
//! resolution, tarball downloads verified by SHA-256, and the cache in
//! `~/.kiln` (or `$KILN_HOME`). See docs/REGISTRY.md.
//!
//! An index is a directory (usually a git repository):
//!
//! ```text
//! config.toml            dl = "dl"   (where tarballs live; relative or https://)
//! packages/<name>        one line per published version:
//!                        <version> sha256:<hex> <deps or -> [yanked]
//! dl/<name>/<name>-<version>.tar.gz
//! ```
//!
//! `deps` is `a=^1.2;b=0.3` (requirements without spaces).

use kiln_parser::manifest::{kiln_home, parse_manifest, registry_src, Manifest};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The index used when neither `KILN_REGISTRY` nor `~/.kiln/config.toml`
/// names one. Not live yet: see docs/REGISTRY.md.
pub const DEFAULT_INDEX: &str = "https://github.com/Cody-will/kiln-index.git";

static OFFLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--offline` / `KILN_OFFLINE=1`: never touch the network.
pub fn set_offline() {
    OFFLINE.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn offline() -> bool {
    OFFLINE.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("KILN_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0")
}

// ----- SHA-256 -----------------------------------------------------------------

/// The SHA-256 digest of `data`, in lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().fold(String::new(), |mut s, x| {
        use std::fmt::Write;
        let _ = write!(s, "{x:08x}");
        s
    })
}

// ----- semver ------------------------------------------------------------------

/// A semantic version: `major.minor.patch[-pre]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: String,
}

impl Version {
    /// Parses `1.2.3` or `1.2.3-beta.1` (build metadata after `+` is ignored).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, p.to_string()),
            Some(_) => return None,
            None => (s, String::new()),
        };
        let mut it = core.split('.');
        let num = |x: Option<&str>| -> Option<u64> {
            let x = x?;
            if x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            x.parse().ok()
        };
        let v = Version {
            major: num(it.next())?,
            minor: num(it.next())?,
            patch: num(it.next())?,
            pre,
        };
        it.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Equal,
        (true, false) => return Greater,
        (false, true) => return Less,
        _ => {}
    }
    let (mut x, mut y) = (a.split('.'), b.split('.'));
    loop {
        match (x.next(), y.next()) {
            (None, None) => return Equal,
            (None, Some(_)) => return Less,
            (Some(_), None) => return Greater,
            (Some(p), Some(q)) => {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Less,
                    (Err(_), Ok(_)) => Greater,
                    _ => p.cmp(q),
                };
                if o != Equal {
                    return o;
                }
            }
        }
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(o.major, o.minor, o.patch))
            .then_with(|| cmp_pre(&self.pre, &o.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Ge,
    Gt,
    Lt,
    Le,
    Eq,
}

/// A version requirement: comparators that must all hold. `1.2` means
/// `^1.2` (compatible updates), as in Cargo.
#[derive(Debug, Clone)]
pub struct Req {
    cmps: Vec<(Op, Version)>,
    pre: bool,
}

impl Req {
    /// Parses `1.2`, `^1.2.3`, `~1.2`, `=1.2.3`, `>=1, <2`, `1.*`, `*`.
    pub fn parse(s: &str) -> Result<Req, String> {
        let bad = || format!("invalid version requirement `{s}`");
        let mut cmps = vec![];
        let mut pre = false;
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(bad());
            }
            let (op, rest) = ["^", "~", ">=", "<=", ">", "<", "="]
                .iter()
                .find_map(|o| part.strip_prefix(o).map(|r| (*o, r.trim())))
                .unwrap_or(("", part));
            if rest == "*" && op.is_empty() {
                continue;
            }
            // Partial versions: `1`, `1.2`, `1.*`, `1.2.*`.
            let fields: Vec<&str> = rest.split('.').collect();
            let full = fields.len() == 3 && fields[2] != "*" && fields[2] != "x";
            let (v, given) = if full {
                let v = Version::parse(rest).ok_or_else(bad)?;
                pre |= !v.pre.is_empty();
                (v, 3)
            } else {
                let mut nums = vec![];
                for f in &fields {
                    if *f == "*" || *f == "x" {
                        break;
                    }
                    nums.push(f.parse::<u64>().map_err(|_| bad())?);
                }
                if nums.is_empty() || fields.len() > 3 {
                    return Err(bad());
                }
                let n = nums.len();
                nums.resize(3, 0);
                (
                    Version {
                        major: nums[0],
                        minor: nums[1],
                        patch: nums[2],
                        pre: String::new(),
                    },
                    n,
                )
            };
            let bump = |major: u64, minor: u64, patch: u64| Version {
                major,
                minor,
                patch,
                pre: String::new(),
            };
            let caret_upper = if v.major > 0 || given == 1 {
                bump(v.major + 1, 0, 0)
            } else if v.minor > 0 || given == 2 {
                bump(0, v.minor + 1, 0)
            } else {
                bump(0, 0, v.patch + 1)
            };
            let tilde_upper = if given == 1 {
                bump(v.major + 1, 0, 0)
            } else {
                bump(v.major, v.minor + 1, 0)
            };
            let partial_upper = match given {
                1 => bump(v.major + 1, 0, 0),
                2 => bump(v.major, v.minor + 1, 0),
                _ => bump(v.major, v.minor, v.patch + 1),
            };
            match op {
                "" | "^" => {
                    cmps.push((Op::Ge, v));
                    cmps.push((Op::Lt, caret_upper));
                }
                "~" => {
                    cmps.push((Op::Ge, v));
                    cmps.push((Op::Lt, tilde_upper));
                }
                "=" if given == 3 => cmps.push((Op::Eq, v)),
                "=" => {
                    cmps.push((Op::Ge, v));
                    cmps.push((Op::Lt, partial_upper));
                }
                ">=" => cmps.push((Op::Ge, v)),
                ">" if given == 3 => cmps.push((Op::Gt, v)),
                ">" => cmps.push((Op::Ge, partial_upper)),
                "<" => cmps.push((Op::Lt, v)),
                "<=" if given == 3 => cmps.push((Op::Le, v)),
                "<=" => cmps.push((Op::Lt, partial_upper)),
                _ => return Err(bad()),
            }
        }
        Ok(Req { cmps, pre })
    }

    /// Whether `v` satisfies every comparator. Pre-releases match only
    /// when the requirement names one.
    pub fn matches(&self, v: &Version) -> bool {
        if !v.pre.is_empty() && !self.pre {
            return false;
        }
        self.cmps.iter().all(|(op, c)| match op {
            Op::Ge => v >= c,
            Op::Gt => v > c,
            Op::Lt => v < c,
            Op::Le => v <= c,
            Op::Eq => v == c,
        })
    }
}

// ----- the index ---------------------------------------------------------------

/// One published version of a package.
#[derive(Debug, Clone)]
pub struct Entry {
    pub version: Version,
    pub checksum: String,
    pub deps: Vec<(String, String)>,
    pub yanked: bool,
}

impl Entry {
    fn line(&self) -> String {
        let deps = if self.deps.is_empty() {
            "-".to_string()
        } else {
            self.deps
                .iter()
                .map(|(n, r)| format!("{n}={}", r.replace(' ', "")))
                .collect::<Vec<_>>()
                .join(";")
        };
        format!(
            "{} {} {deps}{}",
            self.version,
            self.checksum,
            if self.yanked { " yanked" } else { "" }
        )
    }
}

/// An opened index: a local directory (a checkout for git indexes).
pub struct Index {
    /// The index as written in `kiln.lock` sources.
    pub url: String,
    /// Its files on disk.
    pub root: PathBuf,
}

fn is_remote(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
        || url.starts_with("git+")
}

/// The index to use: `KILN_REGISTRY`, then `registry = "..."` in
/// `~/.kiln/config.toml`, then [`DEFAULT_INDEX`].
pub fn default_index_url() -> String {
    if let Ok(u) = std::env::var("KILN_REGISTRY") {
        if !u.is_empty() {
            return normalize(&u);
        }
    }
    if let Ok(text) = std::fs::read_to_string(kiln_home().join("config.toml")) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "registry" {
                    let v = v.trim().trim_matches('"');
                    if !v.is_empty() {
                        return normalize(v);
                    }
                }
            }
        }
    }
    DEFAULT_INDEX.to_string()
}

/// Local paths become absolute `file://` URLs so lock files are stable.
fn normalize(u: &str) -> String {
    if is_remote(u) || u.starts_with("file://") {
        return u.to_string();
    }
    let p = Path::new(u);
    let abs = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    format!("file://{}", abs.display())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{what}: cannot run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

impl Index {
    /// Opens the index at `url`, cloning (or with `refresh`, pulling) a
    /// remote one into `~/.kiln/registry/index/` unless offline.
    pub fn open(url: &str, refresh: bool) -> Result<Index, String> {
        let url = normalize(url);
        if let Some(p) = url.strip_prefix("file://") {
            let root = PathBuf::from(p);
            if !root.join("packages").is_dir() && !root.join("config.toml").is_file() {
                return Err(format!(
                    "{} is not a kiln index (no config.toml or packages/); create one with `kiln publish --init-index {}`",
                    root.display(),
                    root.display()
                ));
            }
            return Ok(Index { url, root });
        }
        let git_url = url.strip_prefix("git+").unwrap_or(&url);
        let dir_name: String = git_url
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let root = kiln_home().join("registry").join("index").join(dir_name);
        if !root.join(".git").is_dir() {
            if offline() {
                return Err(format!("the index {url} is not cached and kiln is offline"));
            }
            std::fs::create_dir_all(root.parent().unwrap())
                .map_err(|e| format!("cannot create {}: {e}", root.display()))?;
            run(
                Command::new("git")
                    .args(["clone", "--depth", "1", "--quiet", git_url])
                    .arg(&root),
                &format!("fetching the index {url}"),
            )?;
        } else if refresh && !offline() {
            run(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(["pull", "--ff-only", "--quiet"]),
                &format!("updating the index {url}"),
            )?;
        }
        Ok(Index { url, root })
    }

    /// Every published version of `name`, oldest first.
    pub fn entries(&self, name: &str) -> Result<Vec<Entry>, String> {
        let path = self.root.join("packages").join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || format!("{}:{}: malformed index line", path.display(), n + 1);
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 3 {
                return Err(bad());
            }
            let version = Version::parse(f[0]).ok_or_else(bad)?;
            let deps = if f[2] == "-" {
                vec![]
            } else {
                f[2].split(';')
                    .map(|d| {
                        d.split_once('=')
                            .map(|(a, b)| (a.to_string(), b.to_string()))
                            .ok_or_else(bad)
                    })
                    .collect::<Result<_, _>>()?
            };
            out.push(Entry {
                version,
                checksum: f[1].to_string(),
                deps,
                yanked: f.get(3) == Some(&"yanked"),
            });
        }
        out.sort_by(|a, b| a.version.cmp(&b.version));
        Ok(out)
    }

    /// Where tarballs live (`dl` in config.toml; default `dl/`).
    fn dl(&self) -> String {
        let text = std::fs::read_to_string(self.root.join("config.toml")).unwrap_or_default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "dl" {
                    return v.trim().trim_matches('"').to_string();
                }
            }
        }
        "dl".into()
    }

    /// The tarball of `name` at `version`, downloaded into the cache.
    fn tarball(&self, name: &str, version: &str) -> Result<PathBuf, String> {
        let file = format!("{name}-{version}.tar.gz");
        let dl = self.dl();
        if is_remote(&dl) {
            let cache = kiln_home().join("registry").join("cache");
            let dest = cache.join(&file);
            if dest.is_file() {
                return Ok(dest);
            }
            if offline() {
                return Err(format!("{file} is not cached and kiln is offline"));
            }
            std::fs::create_dir_all(&cache)
                .map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
            let url = format!("{}/{name}/{file}", dl.trim_end_matches('/'));
            let tmp = cache.join(format!("{file}.part"));
            run(
                Command::new("curl")
                    .args(["-fsSL", "-o"])
                    .arg(&tmp)
                    .arg(&url),
                &format!("downloading {url}"),
            )?;
            std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
            Ok(dest)
        } else {
            let base = Path::new(&dl);
            let base = if base.is_absolute() {
                base.to_path_buf()
            } else {
                self.root.join(base)
            };
            let p = base.join(name).join(&file);
            if !p.is_file() {
                return Err(format!("the index has no tarball {}", p.display()));
            }
            Ok(p)
        }
    }
}

// ----- resolution and fetching ------------------------------------------------

/// A resolved registry package.
#[derive(Debug, Clone)]
pub struct Pick {
    pub name: String,
    pub version: Version,
    pub checksum: String,
    pub source: String,
}

/// The registry requirements of `m` and of its path dependencies
/// (transitively): (name, requirement, who asked).
fn root_reqs(m: &Manifest) -> Result<Vec<(String, String, String)>, String> {
    let mut out = vec![];
    let mut stack = vec![m.clone()];
    let mut seen = vec![m.dir.canonicalize().unwrap_or(m.dir.clone())];
    while let Some(cur) = stack.pop() {
        for (n, r) in &cur.reg {
            out.push((n.clone(), r.clone(), cur.name.clone()));
        }
        for (name, dir) in &cur.deps {
            let canon = dir
                .canonicalize()
                .map_err(|_| format!("dependency `{name}`: no package at {}", dir.display()))?;
            if seen.contains(&canon) {
                continue;
            }
            seen.push(canon);
            stack.push(
                parse_manifest(&dir.join("kiln.toml"))
                    .map_err(|e| format!("dependency `{name}`: {e}"))?,
            );
        }
    }
    Ok(out)
}

/// Locked registry packages from `kiln.lock`: name, version, source, checksum.
fn read_lock(dir: &Path) -> Vec<Pick> {
    let Ok(text) = std::fs::read_to_string(dir.join("kiln.lock")) else {
        return vec![];
    };
    let mut out = vec![];
    let mut cur: Vec<(String, String)> = vec![];
    let flush = |cur: &mut Vec<(String, String)>, out: &mut Vec<Pick>| {
        let get = |k: &str| cur.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone());
        if let (Some(n), Some(v), Some(s), Some(c)) =
            (get("name"), get("version"), get("source"), get("checksum"))
        {
            if let (Some(src), Some(ver)) = (s.strip_prefix("registry+"), Version::parse(&v)) {
                out.push(Pick {
                    name: n,
                    version: ver,
                    checksum: c,
                    source: src.to_string(),
                });
            }
        }
        cur.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            flush(&mut cur, &mut out);
        } else if let Some((k, v)) = line.split_once('=') {
            cur.push((k.trim().to_string(), v.trim().trim_matches('"').to_string()));
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Whether `p` is unpacked in the cache with the expected checksum.
fn cached(p: &Pick) -> bool {
    std::fs::read_to_string(registry_src(&p.name, &p.version.to_string()).join(".kiln-checksum"))
        .is_ok_and(|c| c.trim() == p.checksum)
}

/// Unpacks `p` into the cache, verifying its checksum.
fn fetch(p: &Pick, index: &Index) -> Result<(), String> {
    if cached(p) {
        return Ok(());
    }
    let ver = p.version.to_string();
    let tar = index.tarball(&p.name, &ver)?;
    let bytes = std::fs::read(&tar).map_err(|e| format!("cannot read {}: {e}", tar.display()))?;
    let got = format!("sha256:{}", sha256_hex(&bytes));
    if got != p.checksum {
        return Err(format!(
            "checksum mismatch for {} {ver}: expected {}, got {got} ({})",
            p.name,
            p.checksum,
            tar.display()
        ));
    }
    let dest = registry_src(&p.name, &ver);
    let parent = dest.parent().unwrap();
    let tmp = parent.join(format!(".{}-{ver}.tmp", p.name));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(&tar)
            .arg("-C")
            .arg(&tmp),
        &format!("unpacking {}", tar.display()),
    )?;
    let inner = tmp.join(format!("{}-{ver}", p.name));
    if !inner.join("kiln.toml").is_file() {
        return Err(format!(
            "{} does not contain {}-{ver}/kiln.toml",
            tar.display(),
            p.name
        ));
    }
    std::fs::write(inner.join(".kiln-checksum"), format!("{}\n", p.checksum))
        .and_then(|_| std::fs::write(inner.join(".kiln-source"), format!("{}\n", p.source)))
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&inner, &dest)
        .map_err(|e| format!("cannot move into {}: {e}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Picks one version per package: the locked one when it still matches
/// every requirement (unless `unlock` names it, or is empty with
/// `unlock_all`), otherwise the newest matching, unyanked version.
/// Greedy: requirements are checked against earlier picks, no backtracking.
fn resolve(
    reqs: Vec<(String, String, String)>,
    locked: &[Pick],
    index: &Index,
    unlock: &[String],
    unlock_all: bool,
) -> Result<Vec<Pick>, String> {
    let mut picks: Vec<(Pick, Vec<(String, String)>)> = vec![];
    let mut all: Vec<(String, Req, String, String)> = vec![];
    let mut queue: std::collections::VecDeque<(String, String, String)> = reqs.into();
    while let Some((name, req_s, from)) = queue.pop_front() {
        let req = Req::parse(&req_s).map_err(|e| format!("`{from}` depends on `{name}`: {e}"))?;
        if let Some((p, _)) = picks.iter().find(|(p, _)| p.name == name) {
            if !req.matches(&p.version) {
                let others: Vec<String> = all
                    .iter()
                    .filter(|(n, ..)| *n == name)
                    .map(|(_, _, r, f)| format!("`{f}` wants `{r}`"))
                    .collect();
                return Err(format!(
                    "cannot pick a version of `{name}`: `{from}` wants `{req_s}` but {} {} was picked ({})",
                    name,
                    p.version,
                    others.join(", ")
                ));
            }
            all.push((name, req, req_s, from));
            continue;
        }
        let entries = index.entries(&name)?;
        if entries.is_empty() {
            return Err(format!(
                "`{from}` depends on `{name}`, which is not in the index {}",
                index.url
            ));
        }
        // All requirements on `name` seen so far must hold.
        let ok = |v: &Version| req.matches(v);
        let keep = !unlock_all && !unlock.contains(&name);
        let locked_pick = locked
            .iter()
            .find(|l| l.name == name && keep && ok(&l.version) && l.source == index.url);
        let entry = match locked_pick {
            Some(l) => entries
                .iter()
                .find(|e| e.version == l.version)
                .ok_or_else(|| format!("{name} {} is locked but not in the index", l.version))?,
            None => entries
                .iter()
                .rev()
                .find(|e| !e.yanked && ok(&e.version))
                .ok_or_else(|| {
                    let have: Vec<String> = entries.iter().map(|e| e.version.to_string()).collect();
                    format!(
                        "no version of `{name}` matches `{req_s}` (wanted by `{from}`; the index has {})",
                        have.join(", ")
                    )
                })?,
        };
        for (n, r) in &entry.deps {
            queue.push_back((n.clone(), r.clone(), format!("{name} {}", entry.version)));
        }
        picks.push((
            Pick {
                name: name.clone(),
                version: entry.version.clone(),
                checksum: entry.checksum.clone(),
                source: index.url.clone(),
            },
            entry.deps.clone(),
        ));
        all.push((name, req, req_s, from));
    }
    Ok(picks.into_iter().map(|(p, _)| p).collect())
}

/// Makes sure every registry dependency of `m` is resolved and unpacked,
/// and tells the manifest loader which versions to use. Uses the locked
/// versions when they still satisfy the manifest (no index access when
/// they are cached); `locked` forbids changing them.
pub fn ensure(m: &Manifest, locked_only: bool) -> Result<(), String> {
    let reqs = root_reqs(m)?;
    if reqs.is_empty() {
        return Ok(());
    }
    let lock = read_lock(&m.dir);
    // Fast path: the lock covers every requirement (transitively) and is cached.
    if let Some(picks) = lock_satisfies(&reqs, &lock) {
        let mut need_fetch = picks.iter().filter(|p| !cached(p)).peekable();
        if need_fetch.peek().is_some() {
            for p in need_fetch {
                let index = Index::open(&p.source, false)?;
                fetch(p, &index)?;
            }
        }
        set(&picks);
        return Ok(());
    }
    if locked_only {
        return Err(
            "kiln.lock does not satisfy the registry dependencies in kiln.toml; run without --locked to update it"
                .into(),
        );
    }
    let picks = resolve_and_fetch(m, &lock, &[], false)?;
    set(&picks);
    Ok(())
}

fn set(picks: &[Pick]) {
    kiln_parser::manifest::set_registry_versions(
        picks
            .iter()
            .map(|p| (p.name.clone(), p.version.to_string()))
            .collect(),
    );
}

/// The locked picks, when they satisfy `reqs` and the requirements of the
/// locked packages themselves (read from their cached manifests).
fn lock_satisfies(reqs: &[(String, String, String)], lock: &[Pick]) -> Option<Vec<Pick>> {
    let mut used: Vec<Pick> = vec![];
    let mut queue: Vec<(String, String)> = reqs
        .iter()
        .map(|(n, r, _)| (n.clone(), r.clone()))
        .collect();
    while let Some((n, r)) = queue.pop() {
        let req = Req::parse(&r).ok()?;
        let p = lock.iter().find(|p| p.name == n)?;
        if !req.matches(&p.version) {
            return None;
        }
        if used.iter().any(|u| u.name == n) {
            continue;
        }
        used.push(p.clone());
        // A cached package's own requirements; uncached ones are
        // checked after fetching on the next build.
        let dir = registry_src(&p.name, &p.version.to_string());
        if let Ok(dm) = parse_manifest(&dir.join("kiln.toml")) {
            queue.extend(dm.reg);
        }
    }
    Some(used)
}

fn resolve_and_fetch(
    m: &Manifest,
    lock: &[Pick],
    unlock: &[String],
    unlock_all: bool,
) -> Result<Vec<Pick>, String> {
    let index = Index::open(&default_index_url(), true)?;
    let picks = resolve(root_reqs(m)?, lock, &index, unlock, unlock_all)?;
    for p in &picks {
        fetch(p, &index)?;
    }
    Ok(picks)
}

// ----- commands ----------------------------------------------------------------

fn manifest_here() -> Result<Manifest, String> {
    let path = kiln_parser::manifest::find_manifest(Path::new("."))
        .ok_or("no kiln.toml in this directory or its parents")?;
    parse_manifest(&path)
}

fn exit(r: Result<(), String>) -> std::process::ExitCode {
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

/// Re-resolves and writes kiln.lock by loading the package.
fn relock() -> Result<(), String> {
    crate::package::load(None, true).map_err(|_| "could not load the package".to_string())?;
    Ok(())
}

/// `kiln update [names...]`: newest matching versions (all, or the named).
pub fn update(args: &[String]) -> std::process::ExitCode {
    exit((|| {
        let mut names = vec![];
        for a in args {
            match a.as_str() {
                "--offline" => set_offline(),
                s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
                s => names.push(s.to_string()),
            }
        }
        let m = manifest_here()?;
        if root_reqs(&m)?.is_empty() {
            println!("no registry dependencies");
            return Ok(());
        }
        let lock = read_lock(&m.dir);
        let picks = resolve_and_fetch(&m, &lock, &names, names.is_empty())?;
        for p in &picks {
            match lock.iter().find(|l| l.name == p.name) {
                Some(l) if l.version != p.version => {
                    println!("updated {} {} -> {}", p.name, l.version, p.version)
                }
                None => println!("added {} {}", p.name, p.version),
                _ => {}
            }
        }
        set(&picks);
        relock()
    })())
}

/// `kiln add <name>[@<req>] | --path <dir> <name>`: adds a dependency to kiln.toml.
pub fn add(args: &[String]) -> std::process::ExitCode {
    exit((|| {
        let mut path = None;
        let mut spec = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--path" => path = Some(it.next().ok_or("--path needs a directory")?.clone()),
                "--offline" => set_offline(),
                s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
                s => spec = Some(s.to_string()),
            }
        }
        let m = manifest_here()?;
        let line = match path {
            Some(p) => {
                let dm = parse_manifest(&Path::new(&p).join("kiln.toml"))?;
                let name = spec.unwrap_or(dm.name.clone());
                format!("{name} = {{ path = \"{p}\" }}")
            }
            None => {
                let spec =
                    spec.ok_or("usage: kiln add <name>[@<version>] | kiln add --path <dir>")?;
                let (name, req) = match spec.split_once('@') {
                    Some((n, r)) => (n.to_string(), Some(r.to_string())),
                    None => (spec.clone(), None),
                };
                if !kiln_parser::manifest::is_ident(&name) {
                    return Err(format!("`{name}` is not a valid package name"));
                }
                let index = Index::open(&default_index_url(), true)?;
                let entries = index.entries(&name)?;
                let req = match req {
                    Some(r) => {
                        let parsed = Req::parse(&r)?;
                        if !entries
                            .iter()
                            .any(|e| !e.yanked && parsed.matches(&e.version))
                        {
                            return Err(format!("no version of `{name}` matches `{r}`"));
                        }
                        r
                    }
                    None => entries
                        .iter()
                        .rev()
                        .find(|e| !e.yanked && e.version.pre.is_empty())
                        .map(|e| e.version.to_string())
                        .ok_or_else(|| format!("`{name}` is not in the index {}", index.url))?,
                };
                format!("{name} = \"{req}\"")
            }
        };
        let name = line.split(' ').next().unwrap().to_string();
        if m.deps.iter().any(|(n, _)| *n == name) || m.reg.iter().any(|(n, _)| *n == name) {
            return Err(format!("`{name}` is already a dependency"));
        }
        let path = m.dir.join("kiln.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        match lines.iter().position(|l| l.trim() == "[dependencies]") {
            Some(i) => {
                // After the last entry of the section.
                let mut end = i + 1;
                while end < lines.len() && !lines[end].trim_start().starts_with('[') {
                    end += 1;
                }
                while end > i + 1 && lines[end - 1].trim().is_empty() {
                    end -= 1;
                }
                lines.insert(end, line.clone());
            }
            None => {
                lines.push(String::new());
                lines.push("[dependencies]".into());
                lines.push(line.clone());
            }
        }
        std::fs::write(&path, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
        if let Err(e) = relock() {
            // Leave kiln.toml as it was.
            let _ = std::fs::write(&path, &text);
            return Err(format!("`{line}` was not added: {e}"));
        }
        println!("added {line}");
        Ok(())
    })())
}

/// The files of the package in `dir` that go into its tarball: everything
/// except `target/`, `kiln.lock`, and hidden entries.
fn package_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let rd = std::fs::read_dir(dir.join(&rel)).map_err(|e| e.to_string())?;
        for e in rd {
            let e = e.map_err(|e| e.to_string())?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || (rel.as_os_str().is_empty() && (name == "target" || name == "kiln.lock"))
            {
                continue;
            }
            let r = rel.join(&name);
            if e.file_type().map_err(|e| e.to_string())?.is_dir() {
                stack.push(r);
            } else {
                out.push(r);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Builds `target/package/<name>-<version>.tar.gz`; returns it and its checksum.
pub fn package(m: &Manifest) -> Result<(PathBuf, String), String> {
    let stem = format!("{}-{}", m.name, m.version);
    let out_dir = m.dir.join("target").join("package");
    let stage = out_dir.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    for f in package_files(&m.dir)? {
        let dest = stage.join(&stem).join(&f);
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(m.dir.join(&f), &dest).map_err(|e| e.to_string())?;
    }
    let tar = out_dir.join(format!("{stem}.tar.gz"));
    run(
        Command::new("tar")
            .arg("-czf")
            .arg(&tar)
            .arg("-C")
            .arg(&stage)
            .arg(&stem),
        "creating the tarball",
    )?;
    let _ = std::fs::remove_dir_all(&stage);
    let bytes = std::fs::read(&tar).map_err(|e| e.to_string())?;
    Ok((tar, format!("sha256:{}", sha256_hex(&bytes))))
}

/// `kiln publish [--index <dir|url>] [--dry-run]`, and
/// `kiln publish --init-index <dir>`. Publishing writes to a local index
/// directory (or the local checkout of a git index) and commits there; it
/// never pushes.
pub fn publish(args: &[String]) -> std::process::ExitCode {
    exit((|| {
        let mut index_url = None;
        let mut dry = false;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--index" => index_url = Some(it.next().ok_or("--index needs a value")?.clone()),
                "--dry-run" => dry = true,
                "--init-index" => {
                    let d = PathBuf::from(it.next().ok_or("--init-index needs a directory")?);
                    std::fs::create_dir_all(d.join("packages"))
                        .and_then(|_| std::fs::create_dir_all(d.join("dl")))
                        .map_err(|e| e.to_string())?;
                    if !d.join("config.toml").is_file() {
                        std::fs::write(
                            d.join("config.toml"),
                            "# A kiln package index: see docs/REGISTRY.md.\ndl = \"dl\"\n",
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    println!("created an empty index in {}", d.display());
                    return Ok(());
                }
                s => return Err(format!("unknown argument `{s}`")),
            }
        }
        let m = manifest_here()?;
        if Version::parse(&m.version).is_none() {
            return Err(format!("version `{}` is not semver (x.y.z)", m.version));
        }
        if !m.dir.join("src").join("lib.kn").is_file() {
            return Err("only libraries (src/lib.kn) can be published".into());
        }
        if let Some((n, _)) = m.deps.first() {
            return Err(format!(
                "path dependency `{n}` cannot be published; depend on a registry version instead"
            ));
        }
        for (n, r) in &m.reg {
            Req::parse(r).map_err(|e| format!("dependency `{n}`: {e}"))?;
        }
        // It must build.
        relock()?;
        let (tar, sum) = package(&m)?;
        println!("packaged {} ({sum})", tar.display());
        if dry {
            return Ok(());
        }
        let url = index_url
            .map(|u| normalize(&u))
            .unwrap_or_else(default_index_url);
        let index = Index::open(&url, true)?;
        let ver = Version::parse(&m.version).unwrap();
        if index.entries(&m.name)?.iter().any(|e| e.version == ver) {
            return Err(format!("{} {} is already in the index", m.name, m.version));
        }
        let dl = index.dl();
        if is_remote(&dl) {
            return Err(format!(
                "this index serves tarballs from {dl}; upload {} to {dl}/{}/ first, then add the index line by hand:\n{} {}",
                tar.display(),
                m.name,
                m.name,
                Entry { version: ver, checksum: sum, deps: m.reg.clone(), yanked: false }.line()
            ));
        }
        let dl_dir = index.root.join(&dl).join(&m.name);
        std::fs::create_dir_all(&dl_dir).map_err(|e| e.to_string())?;
        std::fs::copy(&tar, dl_dir.join(tar.file_name().unwrap())).map_err(|e| e.to_string())?;
        let entry = Entry {
            version: ver,
            checksum: sum,
            deps: m.reg.clone(),
            yanked: false,
        };
        let pkg_file = index.root.join("packages").join(&m.name);
        std::fs::create_dir_all(pkg_file.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut text = std::fs::read_to_string(&pkg_file).unwrap_or_default();
        text.push_str(&entry.line());
        text.push('\n');
        std::fs::write(&pkg_file, text).map_err(|e| e.to_string())?;
        if index.root.join(".git").exists() {
            run(
                Command::new("git")
                    .arg("-C")
                    .arg(&index.root)
                    .args(["add", "-A"]),
                "git add",
            )?;
            run(
                Command::new("git").arg("-C").arg(&index.root).args([
                    "commit",
                    "--quiet",
                    "-m",
                    &format!("publish {} {}", m.name, m.version),
                ]),
                "git commit",
            )?;
            println!(
                "published {} {} to the local checkout {} (committed, not pushed; push it to make it public)",
                m.name,
                m.version,
                index.root.display()
            );
        } else {
            println!(
                "published {} {} to {}",
                m.name,
                m.version,
                index.root.display()
            );
        }
        Ok(())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            sha256_hex(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    fn m(req: &str, v: &str) -> bool {
        Req::parse(req)
            .unwrap()
            .matches(&Version::parse(v).unwrap())
    }

    #[test]
    fn semver_requirements() {
        assert!(m("1.2", "1.9.0"));
        assert!(!m("1.2", "2.0.0"));
        assert!(!m("1.2", "1.1.9"));
        assert!(m("^0.2.3", "0.2.9"));
        assert!(!m("^0.2.3", "0.3.0"));
        assert!(!m("^0.0.3", "0.0.4"));
        assert!(m("~1.2.3", "1.2.9"));
        assert!(!m("~1.2.3", "1.3.0"));
        assert!(m("=1.2.3", "1.2.3"));
        assert!(!m("=1.2.3", "1.2.4"));
        assert!(m(">=1, <2", "1.5.0"));
        assert!(!m(">=1, <2", "2.0.0"));
        assert!(m("*", "7.0.0"));
        assert!(m("1.*", "1.4.0"));
        assert!(!m("1.2", "1.3.0-beta"));
        assert!(m(">=1.3.0-alpha", "1.3.0-beta"));
        assert!(Version::parse("1.0.0-alpha").unwrap() < Version::parse("1.0.0").unwrap());
        assert!(
            Version::parse("1.0.0-alpha.2").unwrap() < Version::parse("1.0.0-alpha.10").unwrap()
        );
        assert!(Req::parse("1.x.y.z").is_err());
        assert!(Req::parse("").is_err());
    }
}
