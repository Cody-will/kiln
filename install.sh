#!/bin/sh
# Installs Kiln from this source tree.
#
#   ./install.sh [--prefix DIR] [--no-llvm] [--no-modify-path] [--uninstall]
#
# Layout (the prefix defaults to ~/.kiln; KILN_PREFIX also sets it):
#   <prefix>/bin/kiln             the toolchain (compiler, fmt, test, lsp)
#   <prefix>/lib/kiln/std/        the standard library sources
#   <prefix>/lib/kiln/runtime/    the runtime static libraries
# kiln finds lib/kiln relative to its own executable, so the prefix can be
# moved or copied anywhere. Then <prefix>/bin is added to PATH in your
# shell's startup file (unless --no-modify-path).
#
# Needs: cargo (https://rustup.rs) and a C compiler driver (cc), which kiln
# also uses to link programs. With LLVM 18 present, release builds use the
# optimizing LLVM backend.
set -eu

prefix="${KILN_PREFIX:-$HOME/.kiln}"
llvm=auto
modify_path=1
uninstall=0
while [ $# -gt 0 ]; do
	case "$1" in
	--prefix) prefix="$2"; shift ;;
	--prefix=*) prefix="${1#--prefix=}" ;;
	--no-llvm) llvm=no ;;
	--llvm) llvm=yes ;;
	--no-modify-path) modify_path=0 ;;
	--uninstall) uninstall=1 ;;
	-h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
	rm -f "$prefix/bin/kiln"
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

src="$(cd "$(dirname "$0")" && pwd)"
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
