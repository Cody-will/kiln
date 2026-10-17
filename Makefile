# make install [PREFIX=~/.kiln] [LLVM=no]   build and install kiln
# make uninstall [PREFIX=...]
PREFIX ?= $(HOME)/.kiln
LLVM ?= auto

install:
	./install.sh --prefix "$(PREFIX)" $(if $(filter no,$(LLVM)),--no-llvm,) $(if $(NO_PATH),--no-modify-path,)

uninstall:
	./install.sh --prefix "$(PREFIX)" --uninstall

.PHONY: install uninstall
