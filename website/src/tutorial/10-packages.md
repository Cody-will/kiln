# Packages and modules

## Modules

A module is a namespace of items. You can write one inline, or (more often) in its own file, as the next section shows:

```kiln
mod geometry {
	pub struct Point {
		pub x f64,
		pub y f64,
	}

	pub fun origin() Point {
		return Point { x: 0.0, y: 0.0 }
	}

	fun helper() {}   // private: only visible inside `geometry`
}

import geometry::{Point, origin}

fun main() {
	p := origin()
	q := Point { x: 1.0, y: p.y }
	println("{q.x}")
}
```

- Items are **private** by default. `pub` makes them public, and `pub(pkg)` makes them visible anywhere in the package but not outside it.
- `import a::b` brings in a path, `import a::{X, Y as Z}` imports several names (with renaming), and `import a::*` imports everything public.
- A plain `import` is private to the module that writes it. `pub import a::X` also re-exports `X`, so other code can reach it through this module's path. Paths can start with `self::` or `super::`.
- Paths are separated by `::`. In code you can use a module's items through its name: `geometry::origin()`.

## Modules in files

`mod name` without a body loads the module from a file next to the one that declares it: `name.kn`, or `name/mod.kn` for a module with children of its own. A module declared in `geo.kn` (or `geo/mod.kn`) keeps its child files in `geo/`:

```text
src/
	main.kn        mod geo
	geo/
		mod.kn     pub mod shapes
		shapes.kn  pub fn area(...)
```

```text
// src/main.kn
mod geo

import geo::shapes::area

fn main() {
	println("{area(3)}")
}
```

Each file is checked once, as part of the whole program, so errors point at the file and line they're in.

## Packages

A **package** is a directory with a `kiln.toml` manifest and a `src/` directory. `kiln new` creates one:

```sh
kiln new hello           # a program: src/main.kn
kiln new mathx --lib     # a library: src/lib.kn
```

```toml
[package]
name = "hello"
version = "0.1.0"

[dependencies]
mathx = { path = "../mathx" }
```

Inside a package, the commands need no file name. They find the nearest `kiln.toml`:

| Command | What it does |
|---|---|
| `kiln build [--release]` | builds `target/debug/hello` (or `target/release/hello`); skipped when no source, option, or compiler changed |
| `kiln run [-- args]` | builds (or reuses) and runs it |
| `kiln test` | runs every `@test` in the package's modules |
| `kiln check` | type- and borrow-checks the package |

A **dependency** is a library package. It becomes a top-level module named after the dependency, so `import mathx::add` works in any file. Dependencies of dependencies are loaded too. Only path dependencies exist so far. A dependency's tests don't run as part of your package's tests.

The standard library is imported the same way as before: `import std::fs`, `import std::net::TcpListener`.

<div class="status">
🚧 <strong>Not implemented yet:</strong> the package registry and version requirements (<code>http = "1.2"</code>), git dependencies, the lockfile, and <code>pub(pkg)</code> visibility. Compilation is whole-program: an unchanged package isn't rebuilt, but any change rebuilds all of it.
</div>
