#!/usr/bin/env bash
# Cross-build maked for one target on Linux x86_64 and package it into dist/.
#
# Usage: scripts/ci/package.sh <target> <tag>
#
# Linux musl links with Rust's self-contained rust-lld; Windows uses the box's
# mingw-w64; Apple targets go through cargo-zigbuild. zig and cargo-zigbuild
# are installed into the runner's own tool cache, not system-wide, because the
# raps-ci box is shared with other tenants.
set -euo pipefail

target="$1"
tag="$2"
root="$(cd "$(dirname "$0")/../.." && pwd)"
manifest="$root/rust_make/Cargo.toml"
tools="${RUNNER_TOOL_CACHE:-$root/.tools}/maked-cross"
ZIG_VERSION="0.13.0"
ZIGBUILD_VERSION="0.23.4"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

ensure_zigbuild() {
  mkdir -p "$tools"
  if [ ! -x "$tools/zig-linux-x86_64-$ZIG_VERSION/zig" ]; then
    log "Fetching zig $ZIG_VERSION"
    curl -fsSL "https://ziglang.org/download/$ZIG_VERSION/zig-linux-x86_64-$ZIG_VERSION.tar.xz" | tar -xJ -C "$tools"
  fi
  if [ ! -x "$tools/bin/cargo-zigbuild" ]; then
    log "Installing cargo-zigbuild $ZIGBUILD_VERSION"
    cargo install --locked --root "$tools" cargo-zigbuild --version "$ZIGBUILD_VERSION"
  fi
  export PATH="$tools/bin:$tools/zig-linux-x86_64-$ZIG_VERSION:$PATH"
}

bin="maked"
case "$target" in
  *-linux-musl)
    rustup target add "$target"
    RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes" \
      cargo build --release --locked --target "$target" --manifest-path "$manifest"
    ;;
  x86_64-pc-windows-gnu)
    rustup target add "$target"
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
      cargo build --release --locked --target "$target" --manifest-path "$manifest"
    bin="maked.exe"
    ;;
  universal2-apple-darwin)
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    ensure_zigbuild
    cargo zigbuild --release --locked --target "$target" --manifest-path "$manifest"
    ;;
  *-apple-darwin)
    rustup target add "$target"
    ensure_zigbuild
    cargo zigbuild --release --locked --target "$target" --manifest-path "$manifest"
    ;;
  *)
    echo "unsupported target: $target" >&2; exit 2 ;;
esac

out="$root/rust_make/target/$target/release/$bin"
[ -f "$out" ] || { echo "missing build output $out" >&2; exit 1; }
file "$out" || true

stage="maked-$tag-$target"
rm -rf "$root/dist" && mkdir -p "$root/dist/$stage/completions" "$root/dist/$stage/man"
cp "$out" "$root/dist/$stage/"
cp "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/dist/$stage/"
cp "$root"/completions/maked.* "$root/dist/$stage/completions/"
cp "$root/doc/maked.1" "$root/dist/$stage/man/"

cd "$root/dist"
if [ "$bin" = "maked.exe" ]; then
  zip -9 -q -r "$stage.zip" "$stage"
else
  tar -czf "$stage.tar.gz" "$stage"
fi
rm -rf "$stage"
ls -l
