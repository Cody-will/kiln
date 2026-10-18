#!/bin/sh
# Installs Kiln.
#
#   curl -fsSL https://raw.githubusercontent.com/Cody-will/kiln/main/install.sh | sh
#   ./install.sh [options]          (from a source checkout: builds it)
#
# Piped through sh (or with --binary), it downloads the prebuilt release for
# this machine (x86_64 or aarch64 Linux) from GitHub Releases, and builds
# from source when there is no release for it. Run from a checkout, it
# builds that checkout.
#
# Options:
#   --prefix DIR       install into DIR (default ~/.kiln; KILN_PREFIX also sets it)
#   --version TAG      install release TAG (default: the latest release)
#   --binary           download a release even when run from a checkout
#   --from-source      build from source (clones the repo when not in a checkout)
#   --no-llvm          source builds: Cranelift only, even if LLVM 18 is present
#   --no-kilnc         source builds: skip bootstrapping kilnc
#   --no-modify-path   do not add <prefix>/bin to PATH in shell startup files
#   --uninstall        remove an installation
#
# Layout (kiln finds lib/kiln next to its own executable, so the prefix can
# be moved):
#   <prefix>/bin/kiln             the toolchain (build, run, fmt, test, lsp)
#   <prefix>/lib/kiln/kilnc       the self-hosted compiler, the default code
#                                 generator (needs clang to build programs)
#   <prefix>/lib/kiln/std/        the standard library sources
#   <prefix>/lib/kiln/runtime/    the runtime static libraries
#   <prefix>/lib/kiln/editors/    kiln.vsix and the nvim plugin
#                                 (kiln editor install vscode|nvim)
#
# Building programs needs clang and a linker (lld is used when present);
# source builds also need cargo (https://rustup.rs).
set -eu

prefix="${KILN_PREFIX:-$HOME/.kiln}"
repo="${KILN_REPO:-Cody-will/kiln}"
version=latest
mode=auto
llvm=auto
modify_path=1
uninstall=0
kilnc=auto
while [ $# -gt 0 ]; do
	case "$1" in
	--prefix) prefix="$2"; shift ;;
	--prefix=*) prefix="${1#--prefix=}" ;;
	--version) version="$2"; shift ;;
	--version=*) version="${1#--version=}" ;;
	--binary) mode=binary ;;
	--from-source) mode=source ;;
	--no-llvm) llvm=no ;;
	--llvm) llvm=yes ;;
	--no-modify-path) modify_path=0 ;;
	--no-kilnc) kilnc=no ;;
	--uninstall) uninstall=1 ;;
	-h|--help) sed -n '2,33p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || echo "see https://github.com/$repo/blob/main/install.sh"; exit 0 ;;
	*) echo "install.sh: unknown option $1" >&2; exit 2 ;;
	esac
	shift
done

marker="# added by the Kiln installer"
rc_files() {
	for f in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
		echo "$f"
	done
}

if [ "$uninstall" = 1 ]; then
	rm -f "$prefix/bin/kiln" "$prefix/lib/kiln/kilnc"
	rm -rf "$prefix/lib/kiln"
	rmdir "$prefix/bin" "$prefix/lib" "$prefix" 2>/dev/null || true
	for f in $(rc_files) "$HOME/.config/fish/conf.d/kiln.fish"; do
		[ -f "$f" ] || continue
		if grep -q "$marker" "$f"; then
			grep -v "$marker" "$f" > "$f.kiln-tmp" && mv "$f.kiln-tmp" "$f"
		fi
	done
	rm -f "$HOME/.config/fish/conf.d/kiln.fish"
	echo "Kiln removed from $prefix."
	exit 0
fi

# The source checkout this script sits in, if any (empty when piped to sh).
src=""
case "$0" in
*install.sh) d="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" && [ -f "$d/Cargo.toml" ] && [ -d "$d/compiler" ] && src="$d" ;;
esac
if [ "$mode" = auto ]; then
	if [ -n "$src" ]; then mode=source; else mode=binary; fi
fi

fetch() { # url dest
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --retry 3 -o "$2" "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "$2" "$1"
	else
		echo "error: need curl or wget" >&2
		return 1
	fi
}

install_binary() {
	case "$(uname -s)" in
	Linux) os=linux ;;
	*) echo "No prebuilt Kiln for $(uname -s) yet." >&2; return 1 ;;
	esac
	case "$(uname -m)" in
	x86_64|amd64) arch=x86_64 ;;
	aarch64|arm64) arch=aarch64 ;;
	*) echo "No prebuilt Kiln for $(uname -m) yet." >&2; return 1 ;;
	esac
	asset="kiln-$arch-$os.tar.gz"
	if [ "$version" = latest ]; then
		base="https://github.com/$repo/releases/latest/download"
	else
		base="https://github.com/$repo/releases/download/$version"
	fi
	tmp="$(mktemp -d)"
	echo "Downloading $base/$asset ..."
	if ! fetch "$base/$asset" "$tmp/$asset"; then
		rm -rf "$tmp"
		echo "No release download for $arch Linux (the repository must be public and have a release)." >&2
		return 1
	fi
	if fetch "$base/$asset.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
		want="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
		got="$( (sha256sum "$tmp/$asset" 2>/dev/null || shasum -a 256 "$tmp/$asset") | cut -d' ' -f1)"
		if [ "$want" != "$got" ]; then
			rm -rf "$tmp"
			echo "error: checksum mismatch for $asset" >&2
			exit 1
		fi
	fi
	mkdir -p "$tmp/x" && tar -xzf "$tmp/$asset" -C "$tmp/x"
	root="$(find "$tmp/x" -mindepth 1 -maxdepth 1 -type d | head -1)"
	[ -x "$root/bin/kiln" ] || { rm -rf "$tmp"; echo "error: $asset has no bin/kiln" >&2; exit 1; }
	mkdir -p "$prefix/bin" "$prefix/lib"
	rm -rf "$prefix/lib/kiln.new"
	cp -R "$root/lib/kiln" "$prefix/lib/kiln.new"
	cp "$root/bin/kiln" "$prefix/bin/kiln.new"
	rm -rf "$prefix/lib/kiln"
	mv "$prefix/lib/kiln.new" "$prefix/lib/kiln"
	mv "$prefix/bin/kiln.new" "$prefix/bin/kiln"
	echo "Installed $("$prefix/bin/kiln" --version) into $prefix."
	rm -rf "$tmp"
}

install_source() {
	command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found; install Rust from https://rustup.rs" >&2; exit 1; }
	command -v "${KILN_CC:-${CC:-cc}}" >/dev/null 2>&1 || echo "warning: no C compiler driver (cc) on PATH; kiln needs one to link programs (run \`kiln doctor\` after installing one)" >&2

	features=""
	if [ "$llvm" != no ]; then
		llvm_prefix="${LLVM_SYS_181_PREFIX:-}"
		if [ -z "$llvm_prefix" ]; then
			for d in /usr/lib/llvm-18 /opt/homebrew/opt/llvm@18 /usr/local/opt/llvm@18; do
				[ -x "$d/bin/llvm-config" ] && { llvm_prefix="$d"; break; }
			done
		fi
		if [ -n "$llvm_prefix" ]; then
			export LLVM_SYS_181_PREFIX="$llvm_prefix"
			features="--features llvm-dynamic"
			echo "Using LLVM 18 at $llvm_prefix (optimizing release builds)."
		elif [ "$llvm" = yes ]; then
			echo "error: --llvm given but LLVM 18 was not found (set LLVM_SYS_181_PREFIX)" >&2
			exit 1
		else
			echo "LLVM 18 not found: building with the Cranelift backend only."
		fi
	fi

	echo "Building kiln (release)..."
	# shellcheck disable=SC2086
	(cd "$src" && cargo build --release --locked -p kiln_cli $features)

	mkdir -p "$prefix/bin" "$prefix/lib"
	rm -rf "$prefix/lib/kiln.new"
	cp "$src/target/release/kiln" "$prefix/bin/kiln.new"
	"$prefix/bin/kiln.new" __install-lib "$prefix/lib/kiln.new"
	rm -rf "$prefix/lib/kiln"
	mv "$prefix/lib/kiln.new" "$prefix/lib/kiln"
	mv "$prefix/bin/kiln.new" "$prefix/bin/kiln"
	echo "Installed $prefix/bin/kiln with std and runtime in $prefix/lib/kiln."

	# kilnc: built by the Rust compiler (stage 1), then by itself (stage 2, what
	# gets installed); `kiln build` uses it unless --compiler=rust is given.
	clang="${KILN_CLANG:-clang}"
	if [ "$kilnc" != no ]; then
		if command -v "$clang" >/dev/null 2>&1; then
			echo "Bootstrapping kilnc (the self-hosted compiler)..."
			boot="$src/target/install-kilnc"
			rm -rf "$boot" && mkdir -p "$boot"
			(cd "$src/compiler" && KILN_COMPILER=rust "$prefix/bin/kiln" build --release -o "$boot/stage1" >/dev/null)
			KILN_RT="$prefix/lib/kiln/runtime/libkiln_rt.a" KILNC_RESOLVE_DIR="$src/crates/kiln_resolve" \
				"$boot/stage1" --build --release --c-source "$src/compiler/native/arena.c" "$src/compiler/src/main.kn" -o "$boot/stage2"
			cp "$boot/stage2" "$prefix/lib/kiln/kilnc.new" && mv "$prefix/lib/kiln/kilnc.new" "$prefix/lib/kiln/kilnc"
			echo "Installed $prefix/lib/kiln/kilnc (the default code generator)."
		else
			echo "clang not found: skipping kilnc; kiln uses its built-in compiler."
		fi
	fi

	if [ -d "$src/editors/nvim" ]; then
		mkdir -p "$prefix/lib/kiln/editors"
		rm -rf "$prefix/lib/kiln/editors/nvim"
		cp -R "$src/editors/nvim" "$prefix/lib/kiln/editors/nvim"
		rm -rf "$prefix/lib/kiln/editors/nvim/test"
		vsix="$(ls "$src"/editors/vscode/kiln-*.vsix 2>/dev/null | tail -1)"
		[ -n "$vsix" ] && cp "$vsix" "$prefix/lib/kiln/editors/kiln.vsix"
	fi
}

# Without a checkout, building from source means cloning one.
clone_source() {
	command -v git >/dev/null 2>&1 || { echo "error: building from source needs git" >&2; exit 1; }
	src="$(mktemp -d)/kiln"
	echo "Cloning https://github.com/$repo ..."
	git clone --depth 1 ${KILN_REF:+--branch "$KILN_REF"} "https://github.com/$repo" "$src" ||
		{ echo "error: cannot clone $repo (it must be public, or set KILN_REPO)" >&2; exit 1; }
}

if [ "$mode" = binary ]; then
	if ! install_binary; then
		echo "Building from source instead."
		[ -n "$src" ] || clone_source
		install_source
	fi
else
	[ -n "$src" ] || clone_source
	install_source
fi

if [ "$modify_path" = 1 ]; then
	line="export PATH=\"$prefix/bin:\$PATH\" $marker"
	for f in $(rc_files); do
		case "$f" in
		*/.bashrc) [ -n "${BASH_VERSION:-}" ] || [ -f "$f" ] || continue ;;
		*/.zshrc) [ -f "$f" ] || [ "$(basename "${SHELL:-}")" = zsh ] || continue ;;
		esac
		if ! grep -qs "$marker" "$f"; then
			printf '\n%s\n' "$line" >> "$f"
			echo "Added $prefix/bin to PATH in $f."
		fi
	done
	if [ -d "$HOME/.config/fish" ]; then
		mkdir -p "$HOME/.config/fish/conf.d"
		echo "fish_add_path \"$prefix/bin\" $marker" > "$HOME/.config/fish/conf.d/kiln.fish"
	fi
	case ":$PATH:" in
	*":$prefix/bin:"*) ;;
	*) echo "Open a new shell (or run: export PATH=\"$prefix/bin:\$PATH\") to use kiln." ;;
	esac
fi

echo
"$prefix/bin/kiln" doctor || true
