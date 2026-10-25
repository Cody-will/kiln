#!/bin/sh
# Builds the release tarball for this machine:
#   target/dist/kiln-<arch>-linux.tar.gz (+ .sha256)
# containing kiln-<version>/{bin/kiln, lib/kiln/{kilnc,std,runtime,editors}}.
#
# kiln is built without LLVM so it only needs libc; kilnc (bootstrapped
# twice, so the shipped one is built by itself) is the default code
# generator and does release builds through clang. Needs cargo and clang;
# npm for the VS Code extension (skipped with a warning without it).
# glibc: the binaries need the glibc of the build machine or newer, so
# build releases on an old distribution (CI uses Ubuntu 22.04).
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[ -n "$version" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
case "$(uname -m)" in
x86_64|amd64) arch=x86_64 ;;
aarch64|arm64) arch=aarch64 ;;
*) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
dist="$root/target/dist"
name="kiln-$version"
stage="$dist/$name"
export CARGO_TARGET_DIR="$root/target/dist-cargo"
rm -rf "$stage" && mkdir -p "$stage/bin"

echo "== kiln $version ($arch), without LLVM"
cargo build --release --locked -p kiln_cli
cp "$CARGO_TARGET_DIR/release/kiln" "$stage/bin/kiln"
strip "$stage/bin/kiln" 2>/dev/null || true
"$stage/bin/kiln" __install-lib "$stage/lib/kiln"

echo "== kilnc (stage1 by the Rust compiler, stage2 by stage1)"
boot="$dist/boot"
rm -rf "$boot" && mkdir -p "$boot"
(cd compiler && KILN_COMPILER=rust KILN_RUN_FLAGS= "$stage/bin/kiln" build --release -o "$boot/stage1" >/dev/null)
KILN_RT="$stage/lib/kiln/runtime/libkiln_rt.a" KILNC_RESOLVE_DIR="$root/crates/kiln_resolve" \
	"$boot/stage1" --build --release --c-source compiler/native/arena.c compiler/src/main.kn -o "$boot/kilnc"
cp "$boot/kilnc" "$stage/lib/kiln/kilnc"
strip "$stage/lib/kiln/kilnc" 2>/dev/null || true

echo "== editors"
mkdir -p "$stage/lib/kiln/editors"
cp -R editors/nvim "$stage/lib/kiln/editors/nvim"
rm -rf "$stage/lib/kiln/editors/nvim/test"
if command -v npm >/dev/null 2>&1; then
	(cd editors/vscode && npm ci --silent && rm -f kiln-*.vsix && npx -y @vscode/vsce package --skip-license >/dev/null)
	cp editors/vscode/kiln-*.vsix "$stage/lib/kiln/editors/kiln.vsix"
else
	echo "warning: npm not found; the tarball has no kiln.vsix" >&2
fi
cp LICENSE-MIT LICENSE-APACHE README.md "$stage/"

echo "== smoke test"
t="$(mktemp -d)"
(cd "$t" && "$stage/bin/kiln" new demo >/dev/null && cd demo && "$stage/bin/kiln" run) | grep -q . ||
	{ echo "smoke test failed" >&2; exit 1; }
rm -rf "$t"

tarball="$dist/kiln-$arch-linux.tar.gz"
tar -czf "$tarball" -C "$dist" "$name"
(cd "$dist" && sha256sum "kiln-$arch-linux.tar.gz" > "kiln-$arch-linux.tar.gz.sha256")
echo "built $tarball ($(du -h "$tarball" | cut -f1))"
