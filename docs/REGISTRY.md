# Kiln package registry

Kiln resolves registry dependencies from a **git-based index** (or a plain
directory used as one), records exact versions and sha256 checksums in
`kiln.lock`, and caches sources in `~/.kiln` (override with `KILN_HOME`).

## Commands

- `kiln add name[@req]` / `kiln add name --path dir`: add a dependency, resolve, lock (kiln.toml is rolled back on failure).
- `kiln update [names]`: re-resolve (all, or only the named packages).
- `kiln publish [--index dir] [--dry-run]`: package `src/` + `kiln.toml` into a tarball and add it to a **local** index directory or checkout. For a git index it commits but never pushes.
- `kiln publish --init-index dir`: create an empty index.
- `--offline` / `KILN_OFFLINE=1`: never touch the network; use the cache and the lock.
- `kiln build --locked`: fail if the lock would change.

Requirements: caret (`1.2`, `^1.2`), tilde (`~1.2`), `=`, `>=`, `<`, `*`, and pre-releases.
Dependencies are written `name = "req"`, `name = { version = "req" }` or `name = { path = "dir" }`.
All dependencies (including transitive ones) are visible by name inside each dependency.

## Index layout

```
config.toml               dl = "dl"   (tarball base, relative or URL)
packages/<name>           one line per version:
                          <version> sha256:<hex> <dep=req;dep=req | -> [yanked]
dl/<name>/<name>-<ver>.tar.gz
```

Index selection: `KILN_REGISTRY`, then `registry = "..."` in `~/.kiln/config.toml`,
then the built-in default `https://github.com/Cody-will/kiln-index.git`.
Git indexes are cloned to `~/.kiln/registry/index/` and pulled unless offline;
remote tarballs are fetched with `curl` into `~/.kiln/registry/cache/` and
verified against the index checksum before unpacking to `~/.kiln/registry/src/<name>-<ver>`.

## Standing up the official index (needs owner approval)

The default URL above is a **placeholder; that repository does not exist**.
To make it real, the owner must approve:

1. Creating the public repo `Cody-will/kiln-index` (or another name; then change
   `DEFAULT_INDEX` in `crates/kiln_cli/src/registry.rs`).
2. Initialising it: `kiln publish --init-index ./kiln-index`, commit, push.
3. Tarball hosting: either keep tarballs in the repo (`dl = "dl"`, simplest) or
   point `dl` at release assets / a static host.
4. A publishing policy: publishers run `kiln publish --index ./kiln-index`
   in a checkout and open a pull request; maintainers merge.
