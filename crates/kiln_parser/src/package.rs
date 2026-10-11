//! Loading a program made of several files.
//!
//! `mod name` without a body (in a file at `dir/file.kn`) loads the module
//! from `dir/name.kn` or `dir/name/mod.kn`, Rust's layout. Modules declared
//! in a non-root file `dir/a.kn` live under `dir/a/`. The root file is
//! `src/main.kn` (or `src/lib.kn` for a library) in a package, or any single
//! file.
//!
//! Dependencies (`[dependencies] util = { path = "../util" }` in `kiln.toml`)
//! become top-level modules named after the dependency, loaded from their
//! `src/lib.kn`, so `import util::Thing` works in every file.
//!
//! Each file is parsed at its own span base (`k << 32` for the k-th file; the
//! root file is at 0), so spans never collide, and is registered with
//! [`kiln_lexer::register_source`] so diagnostics show the right file.

use crate::ast::{Item, ItemKind, Visibility};
use crate::Parse;
use kiln_lexer::{Diagnostic, Span};
use std::path::{Path, PathBuf};

/// Span base of the k-th file of a package.
pub fn file_base(k: usize) -> usize {
    k << 32
}

/// A loaded program: the merged syntax tree plus its files.
#[derive(Debug, Clone)]
pub struct Package {
    /// The merged tree (with every file's diagnostics in `errors`).
    pub parse: Parse,
    /// `(span base, path, text)` for each file; the root file is first, at base 0.
    pub files: Vec<(usize, PathBuf, String)>,
}

impl Package {
    /// The file containing byte offset `at` (spans are package-wide).
    pub fn file_of(&self, at: usize) -> Option<&(usize, PathBuf, String)> {
        self.files
            .iter()
            .filter(|(b, _, _)| *b <= at)
            .max_by_key(|(b, _, _)| *b)
    }
}

/// A dependency: its module name and the root file of its source.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// The name it is imported as.
    pub name: String,
    /// Its root file (`<dir>/src/lib.kn`).
    pub root: PathBuf,
}

/// Loads the program rooted at `root`, with `deps` as top-level modules.
/// `overlay` supplies unsaved editor text for files (by path).
pub fn load(
    root: &Path,
    deps: &[Dependency],
    overlay: &dyn Fn(&Path) -> Option<String>,
) -> Result<Package, String> {
    let text = read(root, overlay).map_err(|e| format!("cannot read `{}`: {e}", root.display()))?;
    let mut l = Loader {
        files: Vec::new(),
        errors: Vec::new(),
        overlay,
    };
    let mut parse = l.parse_file(root, text);
    let dir = root.parent().unwrap_or(Path::new(".")).to_path_buf();
    l.expand(&mut parse.file.items, &dir, &mut Vec::new());
    for d in deps {
        if parse
            .file
            .items
            .iter()
            .any(|i| matches!(&i.kind, ItemKind::Mod { name, .. } if name.name == d.name))
        {
            l.errors.push(Diagnostic::error(
                format!("module `{}` has the same name as a dependency", d.name),
                Span::new(0, 0),
            ));
            continue;
        }
        let text = match read(&d.root, overlay) {
            Ok(t) => t,
            Err(e) => {
                l.errors.push(Diagnostic::error(
                    format!(
                        "dependency `{}`: cannot read `{}`: {e}",
                        d.name,
                        d.root.display()
                    ),
                    Span::new(0, 0),
                ));
                continue;
            }
        };
        let mut p = l.parse_file(&d.root, text);
        let ddir = d.root.parent().unwrap_or(Path::new(".")).to_path_buf();
        l.expand(&mut p.file.items, &ddir, &mut Vec::new());
        // A dependency's tests are its own business.
        strip_tests(&mut p.file.items);
        parse.errors.extend(p.errors);
        let span = Span::new(0, 0);
        parse.file.items.push(Item {
            kind: ItemKind::Mod {
                name: crate::ast::Ident {
                    name: d.name.clone(),
                    span,
                },
                items: Some(p.file.items),
            },
            vis: Visibility::Public,
            attrs: Vec::new(),
            doc: None,
            span,
        });
    }
    parse.errors.extend(l.errors);
    Ok(Package {
        parse,
        files: l.files,
    })
}

fn read(path: &Path, overlay: &dyn Fn(&Path) -> Option<String>) -> std::io::Result<String> {
    match overlay(path) {
        Some(t) => Ok(t),
        None => std::fs::read_to_string(path).map(|s| s.replace("\r\n", "\n")),
    }
}

struct Loader<'o> {
    files: Vec<(usize, PathBuf, String)>,
    errors: Vec<Diagnostic>,
    overlay: &'o dyn Fn(&Path) -> Option<String>,
}

impl Loader<'_> {
    fn parse_file(&mut self, path: &Path, text: String) -> Parse {
        let base = file_base(self.files.len());
        if base > 0 {
            let name: &'static str = Box::leak(path.display().to_string().into_boxed_str());
            let t: &'static str = Box::leak(text.clone().into_boxed_str());
            kiln_lexer::register_source(base, name, t);
        }
        let p = crate::parse_at(&text, base);
        self.files.push((base, path.to_path_buf(), text));
        p
    }

    /// Replaces each `mod name` (no body) in `items` with the file's items.
    /// `dir` is where this file's child modules live; `stack` the files
    /// being loaded (to report cycles).
    fn expand(&mut self, items: &mut [Item], dir: &Path, stack: &mut Vec<PathBuf>) {
        for item in items.iter_mut() {
            let ItemKind::Mod { name, items: body } = &mut item.kind else {
                continue;
            };
            if let Some(inner) = body {
                // An inline module's children live in `dir/name/`.
                let sub = dir.join(&name.name);
                self.expand(inner, &sub, stack);
                continue;
            }
            let flat = dir.join(format!("{}.kn", name.name));
            let nested = dir.join(&name.name).join("mod.kn");
            let (path, child_dir) = if (self.overlay)(&flat).is_some() || flat.is_file() {
                (flat, dir.join(&name.name))
            } else if (self.overlay)(&nested).is_some() || nested.is_file() {
                (nested, dir.join(&name.name))
            } else {
                self.errors.push(
                    Diagnostic::error(
                        format!("file not found for module `{}`", name.name),
                        name.span,
                    )
                    .with_help(format!(
                        "create `{}` or `{}`",
                        flat.display(),
                        nested.display()
                    )),
                );
                *body = Some(Vec::new());
                continue;
            };
            let canon = path.canonicalize().unwrap_or(path.clone());
            if stack.contains(&canon)
                || self
                    .files
                    .iter()
                    .any(|(_, p, _)| p.canonicalize().unwrap_or(p.clone()) == canon)
            {
                self.errors.push(Diagnostic::error(
                    format!("module file `{}` is loaded more than once", path.display()),
                    name.span,
                ));
                *body = Some(Vec::new());
                continue;
            }
            let text = match read(&path, self.overlay) {
                Ok(t) => t,
                Err(e) => {
                    self.errors.push(Diagnostic::error(
                        format!("cannot read `{}`: {e}", path.display()),
                        name.span,
                    ));
                    *body = Some(Vec::new());
                    continue;
                }
            };
            let mut p = self.parse_file(&path, text);
            stack.push(canon);
            self.expand(&mut p.file.items, &child_dir, stack);
            stack.pop();
            self.errors.extend(p.errors);
            *body = Some(p.file.items);
        }
    }
}

fn strip_tests(items: &mut Vec<Item>) {
    items.retain(|i| !i.attrs.iter().any(|a| a.name.name == "test"));
    for i in items.iter_mut() {
        if let ItemKind::Mod {
            items: Some(inner), ..
        } = &mut i.kind
        {
            strip_tests(inner);
        }
    }
}
