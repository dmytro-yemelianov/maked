# Top-level Makefile for makeyd (make by Yemelianov Dmytro) & LeanMake
# Installs makeyd, manual page (makeyd.1), and shell completions

PREFIX ?= /usr/local
DESTDIR ?=
BINDIR = $(DESTDIR)$(PREFIX)/bin
MANDIR = $(DESTDIR)$(PREFIX)/share/man/man1
BASHCOMPDIR = $(DESTDIR)$(PREFIX)/share/bash-completion/completions
ZSHCOMPDIR = $(DESTDIR)$(PREFIX)/share/zsh/site-functions
FISHCOMPDIR = $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d

MAKEYD_BIN = rust_make/target/release/makeyd

VERSION ?= 0.1.0
DISTDIR = dist

all: build-rust build-lean

build-rust:
	@echo "==> Building makeyd (optimized release)..."
	@cargo build --release --manifest-path rust_make/Cargo.toml

build-lean:
	@echo "==> Verifying Lean 4 certified formalization..."
	@lake build --dir lean_make

test: test-rust test-lean test-fuzz

test-rust:
	@echo "==> Running makeyd test suite..."
	@cargo test --manifest-path rust_make/Cargo.toml

test-lean:
	@echo "==> Checking Lean 4 formal proofs..."
	@lake build --dir lean_make

test-fuzz:
	@echo "==> Running differential fuzzer vs GNU Make 4.4..."
	@python3 benchmarks/fuzzer/fuzz_runner.py

release-all: release-macos release-musl release-windows checksums
	@echo "==> Multi-platform release packaging complete in $(DISTDIR)/"

release-macos:
	@echo "==> Building macOS release binaries (arm64 & x86_64)..."
	@mkdir -p $(DISTDIR)
	@cargo build --release --target aarch64-apple-darwin --manifest-path rust_make/Cargo.toml
	@cargo build --release --target x86_64-apple-darwin --manifest-path rust_make/Cargo.toml
	@echo "==> Assembling Universal 2 binary..."
	@lipo -create -output $(DISTDIR)/makeyd rust_make/target/aarch64-apple-darwin/release/makeyd rust_make/target/x86_64-apple-darwin/release/makeyd
	@tar -czf $(DISTDIR)/makeyd-v$(VERSION)-apple-darwin-universal.tar.gz -C $(DISTDIR) makeyd
	@rm -f $(DISTDIR)/makeyd
	@tar -czf $(DISTDIR)/makeyd-v$(VERSION)-aarch64-apple-darwin.tar.gz -C rust_make/target/aarch64-apple-darwin/release makeyd
	@tar -czf $(DISTDIR)/makeyd-v$(VERSION)-x86_64-apple-darwin.tar.gz -C rust_make/target/x86_64-apple-darwin/release makeyd

release-musl:
	@echo "==> Building static Linux Musl binaries (x86_64 & aarch64)..."
	@mkdir -p $(DISTDIR)
	@RUSTFLAGS="-C linker=rust-lld" cargo build --release --target x86_64-unknown-linux-musl --manifest-path rust_make/Cargo.toml
	@RUSTFLAGS="-C linker=rust-lld" cargo build --release --target aarch64-unknown-linux-musl --manifest-path rust_make/Cargo.toml
	@tar -czf $(DISTDIR)/makeyd-v$(VERSION)-x86_64-unknown-linux-musl.tar.gz -C rust_make/target/x86_64-unknown-linux-musl/release makeyd
	@tar -czf $(DISTDIR)/makeyd-v$(VERSION)-aarch64-unknown-linux-musl.tar.gz -C rust_make/target/aarch64-unknown-linux-musl/release makeyd

release-windows:
	@echo "==> Building Windows PE32+ binary (x86_64)..."
	@mkdir -p $(DISTDIR)
	@CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=/opt/homebrew/bin/x86_64-w64-mingw32-gcc cargo build --release --target x86_64-pc-windows-gnu --manifest-path rust_make/Cargo.toml
	@cd rust_make/target/x86_64-pc-windows-gnu/release && zip -9 -q ../../../../$(DISTDIR)/makeyd-v$(VERSION)-x86_64-pc-windows-gnu.zip makeyd.exe

checksums:
	@echo "==> Generating SHA-256 checksums..."
	@cd $(DISTDIR) && shasum -a 256 *.tar.gz *.zip > SHA256SUMS.txt
	@cat $(DISTDIR)/SHA256SUMS.txt

install: build-rust
	@echo "==> Installing makeyd binary to $(BINDIR)..."
	@mkdir -p $(BINDIR)
	@cp -f $(MAKEYD_BIN) $(BINDIR)/makeyd
	@chmod 755 $(BINDIR)/makeyd

	@echo "==> Installing Unix manual page to $(MANDIR)..."
	@mkdir -p $(MANDIR)
	@cp -f doc/makeyd.1 $(MANDIR)/makeyd.1
	@chmod 644 $(MANDIR)/makeyd.1

	@echo "==> Installing shell completions..."
	@mkdir -p $(BASHCOMPDIR) && cp -f completions/makeyd.bash $(BASHCOMPDIR)/makeyd
	@mkdir -p $(ZSHCOMPDIR) && cp -f completions/makeyd.zsh $(ZSHCOMPDIR)/_makeyd
	@mkdir -p $(FISHCOMPDIR) && cp -f completions/makeyd.fish $(FISHCOMPDIR)/makeyd.fish

uninstall:
	@echo "==> Uninstalling makeyd..."
	@rm -f $(BINDIR)/makeyd
	@rm -f $(MANDIR)/makeyd.1
	@rm -f $(BASHCOMPDIR)/makeyd
	@rm -f $(ZSHCOMPDIR)/_makeyd
	@rm -f $(FISHCOMPDIR)/makeyd.fish

clean:
	@cargo clean --manifest-path rust_make/Cargo.toml
	@lake clean --dir lean_make
	@rm -rf $(DISTDIR)

.PHONY: all build-rust build-lean test test-rust test-lean test-fuzz install uninstall clean release-all release-macos release-musl release-windows checksums
