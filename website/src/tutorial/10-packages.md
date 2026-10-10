# Packages and modules

## Modules

Every file is a module, and so is every directory with a `mod.kn`. You can also write a module inline:

```kiln
mod geometry {
	pub struct Point {
		pub x: f64,
		pub y: f64,
	}

	pub fn origin() -> Point {
		return Point { x: 0.0, y: 0.0 }
	}

	fn helper() {}   // private: only visible inside `geometry`
}

import geometry::{Point, origin}

fn main() {
	p := origin()
	q := Point { x: 1.0, y: p.y }
	println("{q.x}")
}
```

- Items are **private** by default. `pub` makes them public, and `pub(pkg)` makes them visible anywhere in the package but not outside it.
- `import a::b` brings in a path, `import a::{X, Y as Z}` imports several names (with renaming), and `import a::*` imports everything public.
- Paths are separated by `::`. In code you can use a module's items through its name: `geometry::origin()`.

## Packages

A **package** is a directory with a `kiln.toml` manifest:

```toml
[package]
name = "webapp"
version = "0.1.0"
edition = "2027"

[dependencies]
http = "1.2"
json = { git = "https://github.com/kiln-lang/json", tag = "v0.3" }
```

The standard library is imported the same way: `import std::fs`, `import std::net::TcpListener`.

<div class="status">
🚧 <strong>Not implemented yet:</strong> multi-file modules (<code>mod.kn</code> directories), <code>kiln.toml</code>, dependencies, the lockfile, and the package registry. Today <code>kiln check</code> works on a single file, and inline <code>mod</code> blocks, <code>import</code>, and visibility are fully resolved within it. The standard library is fully type-checked: see the <a href="../stdlib.html">standard library reference</a>.
</div>
