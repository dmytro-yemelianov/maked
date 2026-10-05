#!/usr/bin/env bash
# Which parts of maked does anything compare against GNU make?
#
# Builds an instrumented maked and runs only the checks that compare it with
# GNU make or the Lean model: rust_make/tests/gnu_compat_tests.rs and the
# fuzzers (and, with --realworld, the six real projects). Code those never
# reach is code whose GNU behaviour nothing checks; that is where the last
# rounds' bugs were (rules without a recipe, `t: ; recipe`, directories in
# pattern rules).
#
# Usage: scripts/coverage.sh [--realworld] [--all-tests]
#   --all-tests  also run maked's own tests (not differential), for contrast
# Output: target/coverage/{summary.txt,functions.tsv,html/}
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
realworld=0
all_tests=0
for a in "$@"; do
  case "$a" in
    --realworld) realworld=1 ;;
    --all-tests) all_tests=1 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

cd "$root/rust_make"
# A separate target dir, so the normal release binary is left alone.
export CARGO_TARGET_DIR="$root/rust_make/target/cov"
eval "$(cargo llvm-cov show-env --sh 2>/dev/null)"
cargo llvm-cov clean --workspace
cargo build --release
export MAKED_BIN="$CARGO_LLVM_COV_TARGET_DIR/release/maked"
[ -x "$MAKED_BIN" ] || { echo "no instrumented binary at $MAKED_BIN" >&2; exit 1; }

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
cd "$root"
log "GNU-differential tests"
cargo test --release --manifest-path rust_make/Cargo.toml --test gnu_compat_tests -q
if [ "$all_tests" = 1 ]; then
  log "maked's own tests (not differential)"
  cargo test --release --manifest-path rust_make/Cargo.toml -q
fi
log "fuzzers"
python3 benchmarks/fuzzer/fuzz_runner.py 100 | tail -1
python3 benchmarks/fuzzer/expr_fuzz.py 300 | tail -1
python3 benchmarks/fuzzer/directive_fuzz.py 200 | tail -1
python3 benchmarks/fuzzer/pattern_fuzz.py 150 | tail -1
python3 benchmarks/fuzzer/remake_fuzz.py 80 | tail -1
python3 benchmarks/fuzzer/cache_fuzz.py 5 | tail -1
python3 benchmarks/fuzzer/schedule_fuzz.py 10 | tail -1
if [ "$realworld" = 1 ]; then
  log "real projects"
  python3 benchmarks/realworld/run.py | tail -1
  git checkout -q benchmarks/realworld/report.json 2>/dev/null || true
fi
git checkout -q benchmarks/fuzzer/oracle_3way_report.json 2>/dev/null || true

out="$root/rust_make/target/coverage"
mkdir -p "$out"
cd "$root/rust_make"
cargo llvm-cov report --release --summary-only | tee "$out/summary.txt"
cargo llvm-cov report --release --html --output-dir "$out/html" >/dev/null
cargo llvm-cov report --release --json --output-path "$out/coverage.json"
python3 - "$out" <<'EOF'
import json, sys, re
out = sys.argv[1]
data = json.load(open(f"{out}/coverage.json"))["data"][0]
rows = []
for fn in data["functions"]:
    name = fn["name"]
    files = [f for f in fn["filenames"] if "/rust_make/src/" in f]
    if not files:
        continue
    regions = [r for r in fn["regions"] if r[-1] == 0]  # code regions
    total = len(regions)
    hit = sum(1 for r in regions if r[4] > 0)
    rows.append((total - hit, total, name, files[0].split("/rust_make/")[1], regions[0][0] if regions else 0))
with open(f"{out}/functions.tsv", "w") as f:
    f.write("missed\ttotal\tfile:line\tfunction\n")
    for missed, total, name, file, line in sorted(rows, reverse=True):
        f.write(f"{missed}\t{total}\t{file}:{line}\t{name}\n")
print(f"per-function regions: {out}/functions.tsv; HTML: {out}/html/index.html")
EOF
