# Making the repository public

The GitHub repository is **public**. This checklist tracks the remaining
optional polish (Marketplace, AUR, Pages automation) after the in-tree
public-readiness pieces (README, dual MIT/Apache licenses, CONTRIBUTING,
SECURITY, CODE_OF_CONDUCT, CI, website book) landed.

Do **not** ask the bots to change GitHub visibility, enable Pages, or
publish marketplace/AUR packages unless you explicitly approve each step.

## Already done in-tree

- [x] Dual license: `LICENSE-MIT` + `LICENSE-APACHE` (README points at both)
- [x] `CONTRIBUTING.md`
- [x] `SECURITY.md`
- [x] `CODE_OF_CONDUCT.md`
- [x] README install one-liner (`curl … | sh`; public releases on GitHub)
- [x] Website / mdBook under `website/` (`mdbook build website`)
- [x] `.github/workflows/pages.yml` (manual `workflow_dispatch` only)
- [x] Linux CI (`.github/workflows/ci.yml`) — green when Actions billing allows
- [x] kilnc as daily-driver codegen; Rust path deprecated (see `docs/SELF_HOSTING.md`)

## Your manual steps

1. **Make the GitHub repository public**  
   Settings → General → Danger Zone → Change repository visibility → Public.

2. **Create and push the `v0.1.0` tag** (or the first real release tag):
   ```sh
   git tag -a v0.1.0 -m "kiln v0.1.0"
   git push origin v0.1.0
   ```
   That should trigger `.github/workflows/release.yml` to build and upload
   release tarballs (including the `kilnc` bootstrap seed). Confirm the
   release assets on the GitHub Releases page.

3. **Enable GitHub Pages** (needs a public repo on the free plan, or a
   paid plan for private Pages):
   - Settings → Pages → Source: **GitHub Actions**
   - Actions → *Deploy website* → *Run workflow*  
   Do not rely on the bot to enable Pages; the workflow is
   `workflow_dispatch` only so a private repo never publishes by accident.
   Documented also in `website/README.md`.

4. **Publish the VS Code extension**  
   Build the VSIX (`make vsix` / `editors/vscode`), then publish to the
   Marketplace / Open VSX with your publisher account. README already
   mentions `kiln editor install vscode` for sideloading.

5. **Neovim notes**  
   Point users at `editors/nvim` (filetype, tree-sitter queries, LSP).
   Optional: submit a PR to `nvim-treesitter` once the parser is stable.

6. **AUR package**  
   `kiln-bin` PKGBUILD is in-tree (see release / packaging docs). Push to
   the AUR under your maintainer account after the first public release
   tarball exists.

7. **Sanity checks after going public**
   - `curl -fsSL https://raw.githubusercontent.com/<owner>/kiln/main/install.sh | sh`
   - `kiln doctor` shows kilnc
   - Website URL resolves (Pages)
   - CI still green on `main`

## Out of scope for bots unless you ask

- Changing repository visibility
- Enabling GitHub Pages in repo settings
- Creating the release tag
- Publishing VSIX / AUR / Homebrew taps (needs `VSCE_PAT` / `OVSX_TOKEN` in the environment or GitHub Actions secrets; until then the `.vsix` is a release asset and `kiln editor install vscode` sideloads it)
- Tweeting or posting announcements
