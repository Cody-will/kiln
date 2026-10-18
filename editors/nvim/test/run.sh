#!/bin/sh
# Headless test of the nvim plugin: filetype, tree-sitter highlighting,
# indentation, folding, and `kiln lsp` diagnostics.
#   editors/nvim/test/run.sh [path/to/kiln]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
plugin=$(dirname "$here")
ts="$plugin/../tree-sitter-kiln"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/rtp/parser"
cc -O1 -shared -fPIC -I"$ts/src" "$ts/src/parser.c" -o "$tmp/rtp/parser/kiln.so"
# The plugin's queries must be the grammar's.
for q in highlights indents folds; do
	cmp -s "$ts/queries/$q.scm" "$plugin/queries/kiln/$q.scm" || { echo "queries/kiln/$q.scm is out of date (make nvim-queries)"; exit 1; }
done
cp "$here/sample.kn" "$tmp/main.kn"
cd "$tmp"
KILN_TEST_BIN="${1:-kiln}" XDG_CONFIG_HOME="$tmp/cfg" XDG_DATA_HOME="$tmp/data" XDG_STATE_HOME="$tmp/state" \
	nvim --headless --clean \
	--cmd "set rtp^=$plugin,$tmp/rtp" --cmd "runtime! ftdetect/*.lua plugin/kiln.lua" \
	-c "luafile $here/headless.lua" main.kn
