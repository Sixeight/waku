PREFIX ?= $(HOME)/.local

.PHONY: install uninstall

install:
	cargo build --release
	install -d $(PREFIX)/bin
	install -m 755 target/release/git-waku $(PREFIX)/bin/git-waku

uninstall:
	rm -f $(PREFIX)/bin/git-waku
