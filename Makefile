# make install [PREFIX=~/.kiln] [LLVM=no]   build and install kiln
# make uninstall [PREFIX=...]
PREFIX ?= $(HOME)/.kiln
LLVM ?= auto

install:
	./install.sh --prefix "$(PREFIX)" $(if $(filter no,$(LLVM)),--no-llvm,) $(if $(NO_PATH),--no-modify-path,)

uninstall:
	./install.sh --prefix "$(PREFIX)" --uninstall

.PHONY: install uninstall

# make bootstrap   build the self-hosted compiler with itself (needs clang)
#   stage1: compiler/ built by the Rust `kiln`
#   stage2: compiler/ built by stage1 (kilnc --build: LLVM IR, then clang)
#   stage3: compiler/ built by stage2; must be identical to stage2
BOOT := target/bootstrap
KILN := target/debug/kiln

bootstrap:
	cargo build -p kiln_cli
	cd compiler && KILN_RUN_FLAGS= ../$(KILN) build
	rm -rf $(BOOT)
	mkdir -p $(BOOT)
	cp compiler/target/debug/kilnc $(BOOT)/stage1
	$(KILN) runtime --write $(BOOT)/rt
	KILN_RT=$(BOOT)/rt/libkiln_rt.a KILNC_RESOLVE_DIR=crates/kiln_resolve \
		$(BOOT)/stage1 --build --c-source compiler/native/arena.c compiler/src/main.kn -o $(BOOT)/stage2
	KILN_RT=$(BOOT)/rt/libkiln_rt.a KILNC_RESOLVE_DIR=crates/kiln_resolve \
		$(BOOT)/stage2 --build --c-source compiler/native/arena.c compiler/src/main.kn -o $(BOOT)/stage3
	cd $(BOOT) && for f in stage2.*.ll; do cmp $$f stage3.$${f#stage2.}; done
	cmp $(BOOT)/stage2 $(BOOT)/stage3
	@echo "bootstrap OK: stage2 and stage3 are identical"

.PHONY: bootstrap

# make dist          the release tarball for this machine (tools/release.sh)
# make vsix          the VS Code extension package (editors/vscode/kiln-*.vsix)
# make nvim-queries  copy the tree-sitter queries into the nvim plugin
# make editors-test  the VS Code grammar test and the headless nvim test
dist:
	tools/release.sh

vsix:
	cd editors/vscode && npm ci && rm -f kiln-*.vsix && npx -y @vscode/vsce package

nvim-queries:
	cp editors/tree-sitter-kiln/queries/highlights.scm editors/tree-sitter-kiln/queries/indents.scm \
		editors/tree-sitter-kiln/queries/folds.scm editors/nvim/queries/kiln/

editors-test:
	cd editors/vscode && npm test
	editors/nvim/test/run.sh $(abspath target/release/kiln)

.PHONY: dist vsix nvim-queries editors-test
