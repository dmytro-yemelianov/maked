#!/usr/bin/env bash
# Smoke-test the Windows build under Wine 10 (Debian trixie, amd64 container).
# Wine 8 cannot run any Rust >= 1.78 binary: it lacks bcryptprimitives.dll.
# The raps-ci box has no Wine, so run this by hand before a release, e.g.
# on macOS with Docker (amd64 emulation) or any Linux host with Docker.
#
# Usage: scripts/ci/windows-wine-smoke.sh path/to/maked.exe
set -euo pipefail
exe="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
# Under $HOME: Docker Desktop/colima share it, but not /var/folders or /tmp.
work="$(mktemp -d "${HOME}/.maked-wine-smoke.XXXXXX")"
trap 'rm -rf "$work"' EXIT
cp "$exe" "$work/maked.exe"
mkdir -p "$work/t"
printf 'all: out.txt\n\t@echo built-all\nout.txt: in.txt\n\tcopy in.txt out.txt\n' > "$work/t/Makefile"
printf 'all: a b\na:\n\t@exit 3\nb:\n\t@echo b-ok\n' > "$work/t/K.mk"
printf 'all: x y z\nx y z:\n\t@echo job-$@\n' > "$work/t/J.mk"
echo hello > "$work/t/in.txt"
docker run --rm --platform linux/amd64 -v "$work:/w" -w /w/t debian:trixie-slim bash -c '
  set -e
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends wine wine64 >/dev/null
  export WINEPREFIX=/tmp/wp WINEDEBUG=-all
  wineboot -i >/dev/null 2>&1 || true
  run() { wine /w/maked.exe "$@" 2>&1 | tr -d "\r"; }
  fails=0
  check() { if eval "$2"; then echo "ok    $1"; else echo "FAIL  $1"; fails=$((fails+1)); fi; }
  [ -f /w/maked.exe ] || { echo "FAIL  maked.exe not visible in the container"; exit 1; }
  check "--version"                  "run --version | grep -q \"^maked \""
  check "build with cmd.exe recipes" "run | grep -q built-all && grep -q hello out.txt"
  check "null build"                 "run | grep -q built-all"
  check "-k keeps going"             "run -k -f K.mk | grep -q b-ok"
  check "-j4"                        "[ \"\$(run -j4 -f J.mk | grep -c job-)\" = 3 ]"
  exit $fails
'
