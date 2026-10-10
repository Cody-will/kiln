# Kiln website and book

This directory holds the Kiln website: a landing page, installation instructions, a step-by-step tutorial, and the language specification (included from `docs/SPEC.md`). It is a static [mdBook](https://rust-lang.github.io/mdBook/) site, so no server is needed.

## Build

```sh
cargo install mdbook        # or download a release binary from GitHub
cd website
mdbook build                # outputs to website/book/
mdbook serve --open         # live-reloading preview at http://localhost:3000
```

Open `website/book/index.html` directly in a browser to view the built site offline.

## Layout

| Path | Contents |
|---|---|
| `book.toml` | mdBook configuration |
| `src/SUMMARY.md` | Table of contents |
| `src/index.md` | Landing page |
| `src/install.md`, `src/status.md` | Installation and implementation status |
| `src/tutorial/` | The tutorial chapters |
| `src/spec.md` | Includes `../docs/SPEC.md` |
| `theme/` | Extra CSS, plus JS that highlights `kiln` code blocks with the Rust highlighter |

## Writing guidelines

- Every ` ```kiln ` block must pass `kiln check`. The test `website_examples_type_check` in `crates/kiln_typeck` enforces this. Use ` ```text ` for intentionally wrong code and for compiler output.
- Mark anything specified but not yet implemented with a `<div class="status">🚧 ...</div>` callout, and update `src/status.md` when a milestone lands.
- Follow `kiln fmt` style in examples: tabs, same-line braces, and trailing commas on multi-line lists.

## Publishing

`.github/workflows/pages.yml` builds the book and deploys it to GitHub Pages. It runs **only when triggered manually** (Actions → *Deploy website* → *Run workflow*), because the repository is private. GitHub Pages for private repositories needs a paid plan, and the published site would be public. Before the first run, enable Pages with the source set to "GitHub Actions" in the repository settings.
