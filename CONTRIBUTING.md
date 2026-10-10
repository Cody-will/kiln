# Contributing to Kiln

Thanks for your interest! Kiln is in early development, so the design is still moving. Read [docs/SPEC.md](docs/SPEC.md) before proposing language changes.

## Setup

```sh
rustup component add rustfmt clippy
cargo build
cargo test
```

## Before you open a pull request

CI runs all three of these; please run them locally too:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Guidelines

- **Tests:** every lexer or parser change needs unit tests (`crates/*/src/tests.rs`). New syntax should also show up in an `examples/*.kn` file, since those are parsed in tests.
- **Docs:** public items need doc comments (`missing_docs` is enabled).
- **Diagnostics:** error messages should say what was expected and what was found, and point at a precise span. Add a test asserting the exact message.
- **Dependencies:** the front end has zero external dependencies. Discuss before adding one.
- **Language changes:** open an issue first. Update `docs/SPEC.md` in the same PR as the implementation.
- **Commits:** keep them small and focused, with an imperative subject line (`parser: support where clauses on enums`).

## License

By contributing, you agree that your contributions will be dual-licensed under MIT OR Apache-2.0, as described in the README.

## Website and tutorial

The site in `website/` is an mdBook. Preview it with `mdbook serve website`. Every ` ```kiln ` code block must pass `kiln check` (enforced by `cargo test`). Mark features that are not implemented yet with a 🚧 status callout, and update `website/src/status.md` when a milestone lands. Deploying to GitHub Pages is manual (`.github/workflows/pages.yml`).
