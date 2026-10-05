# Top-level Makefile for maked (make + ed: Yemelianov (Emelyanov) Dmytro) & LeanMake
# Installs maked, manual page (maked.1), and shell completions

PREFIX ?= /usr/local
DESTDIR ?=
BINDIR = $(DESTDIR)$(PREFIX)/bin
MANDIR = $(DESTDIR)$(PREFIX)/share/man/man1
BASHCOMPDIR = $(DESTDIR)$(PREFIX)/share/bash-completion/completions
ZSHCOMPDIR = $(DESTDIR)$(PREFIX)/share/zsh/site-functions
FISHCOMPDIR = $(DESTDIR)$(PREFIX)/share/fish/vendor_completions.d

MAKED_BIN = rust_make/target/release/maked

VERSION ?= 0.1.0
DISTDIR = dist

all: build-rust build-lean

build-rust:
	@echo "==> Building maked (optimized release)..."
	@cargo build --release --manifest-path rust_make/Cargo.toml

build-lean:
	@echo "==> Verifying Lean 4 certified formalization..."
	@lake build --dir lean_make

test: test-rust test-lean test-fuzz

test-rust:
	@echo "==> Running maked test suite..."
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
	@lipo -create -output $(DISTDIR)/maked rust_make/target/aarch64-apple-darwin/release/maked rust_make/target/x86_64-apple-darwin/release/maked
	@tar -czf $(DISTDIR)/maked-v$(VERSION)-apple-darwin-universal.tar.gz -C $(DISTDIR) maked
	@rm -f $(DISTDIR)/maked
	@tar -czf $(DISTDIR)/maked-v$(VERSION)-aarch64-apple-darwin.tar.gz -C rust_make/target/aarch64-apple-darwin/release maked
	@tar -czf $(DISTDIR)/maked-v$(VERSION)-x86_64-apple-darwin.tar.gz -C rust_make/target/x86_64-apple-darwin/release maked

release-musl:
	@echo "==> Building static Linux Musl binaries (x86_64 & aarch64)..."
	@mkdir -p $(DISTDIR)
	@RUSTFLAGS="-C linker=rust-lld" cargo build --release --target x86_64-unknown-linux-musl --manifest-path rust_make/Cargo.toml
	@RUSTFLAGS="-C linker=rust-lld" cargo build --release --target aarch64-unknown-linux-musl --manifest-path rust_make/Cargo.toml
	@tar -czf $(DISTDIR)/maked-v$(VERSION)-x86_64-unknown-linux-musl.tar.gz -C rust_make/target/x86_64-unknown-linux-musl/release maked
	@tar -czf $(DISTDIR)/maked-v$(VERSION)-aarch64-unknown-linux-musl.tar.gz -C rust_make/target/aarch64-unknown-linux-musl/release maked

release-windows:
	@echo "==> Building Windows PE32+ binary (x86_64)..."
	@mkdir -p $(DISTDIR)
	@CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=/opt/homebrew/bin/x86_64-w64-mingw32-gcc cargo build --release --target x86_64-pc-windows-gnu --manifest-path rust_make/Cargo.toml
	@cd rust_make/target/x86_64-pc-windows-gnu/release && zip -9 -q ../../../../$(DISTDIR)/maked-v$(VERSION)-x86_64-pc-windows-gnu.zip maked.exe

checksums:
	@echo "==> Generating SHA-256 checksums..."
	@cd $(DISTDIR) && shasum -a 256 *.tar.gz *.zip > SHA256SUMS.txt
	@cat $(DISTDIR)/SHA256SUMS.txt

install: build-rust
	@echo "==> Installing maked binary to $(BINDIR)..."
	@mkdir -p $(BINDIR)
	@cp -f $(MAKED_BIN) $(BINDIR)/maked
	@chmod 755 $(BINDIR)/maked

	@echo "==> Installing Unix manual page to $(MANDIR)..."
	@mkdir -p $(MANDIR)
	@cp -f doc/maked.1 $(MANDIR)/maked.1
	@chmod 644 $(MANDIR)/maked.1

	@echo "==> Installing shell completions..."
	@mkdir -p $(BASHCOMPDIR) && cp -f completions/maked.bash $(BASHCOMPDIR)/maked
	@mkdir -p $(ZSHCOMPDIR) && cp -f completions/maked.zsh $(ZSHCOMPDIR)/_maked
	@mkdir -p $(FISHCOMPDIR) && cp -f completions/maked.fish $(FISHCOMPDIR)/maked.fish

uninstall:
	@echo "==> Uninstalling maked..."
	@rm -f $(BINDIR)/maked
	@rm -f $(MANDIR)/maked.1
	@rm -f $(BASHCOMPDIR)/maked
	@rm -f $(ZSHCOMPDIR)/_maked
	@rm -f $(FISHCOMPDIR)/maked.fish

clean:
	@cargo clean --manifest-path rust_make/Cargo.toml
	@lake clean --dir lean_make
	@rm -rf $(DISTDIR)

.PHONY: all build-rust build-lean test test-rust test-lean test-fuzz install uninstall clean release-all release-macos release-musl release-windows checksums
