#!/usr/bin/env bash
# Null-build / cold-build comparison: maked vs GNU make vs Ninja.
#
# Each tool gets its own copy of the generated tree and builds it once before
# timing, so every null build measured is a genuine "nothing to do" run
# (Ninja in particular needs its own .ninja_log to be up to date). The goal
# is always `all`: in some generated Makefiles `all` is not the first rule,
# and make would otherwise build only the first target.
#
# Usage: benchmarks/scalability/fair_bench.sh [out.json]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
maked="$root/rust_make/target/release/maked"
gmake="${GMAKE:-$(command -v gmake || command -v make)}"
ninja="${NINJA:-$(command -v ninja)}"
out="${1:-$root/benchmarks/scalability/fair_bench.json}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cargo build --release --quiet --manifest-path "$root/rust_make/Cargo.toml"

scenarios="modular:1000 diamond:2500 modular:5000 modular:10000 deep:2000 wide:5000"
results=()
for sc in $scenarios; do
  topo="${sc%%:*}"; size="${sc##*:}"; label="${topo}_${size}"
  echo "==> $label"
  for tool in maked gmake ninja; do
    python3 "$root/benchmarks/scalability/generate_massive_dag.py" \
      --topology "$topo" --size "$size" --out "$work/$label-$tool" >/dev/null
  done
  # Cold build, -j8: 5 runs, outputs removed before each (every copy carries
  # the same build.ninja, so `ninja -t clean` works for all three). The last
  # run leaves each tree up to date for the null-build pass.
  hyperfine --runs 5 --export-json "$work/$label-cold.json" -N \
    --prepare "$ninja -C $work/$label-maked -t clean" \
    --prepare "$ninja -C $work/$label-gmake -t clean" \
    --prepare "$ninja -C $work/$label-ninja -t clean" \
    -n maked "$maked -C $work/$label-maked -j8 all" \
    -n gmake "$gmake -C $work/$label-gmake -j8 all" \
    -n ninja "$ninja -C $work/$label-ninja -j8 all" >/dev/null
  # Null builds: warmed, 20 runs.
  hyperfine --warmup 3 --runs 20 --export-json "$work/$label-null.json" -N \
    -n maked "$maked -C $work/$label-maked -j8 all" \
    -n gmake "$gmake -C $work/$label-gmake -j8 all" \
    -n ninja "$ninja -C $work/$label-ninja -j8 all" | grep -E "^ *Time|±|faster" || true
  rss=$(/usr/bin/time -l "$maked" -C "$work/$label-maked" -j8 all 2>&1 >/dev/null | awk '/maximum resident/ {print $1}' || true)
  results+=("{\"scenario\":\"$label\",\"cold\":$(cat "$work/$label-cold.json"),\"null\":$(cat "$work/$label-null.json"),\"maked_null_peak_rss_bytes\":${rss:-null}}")
done

# Real-world workload: Lua 5.4.9 (recursive $(MAKE), real C compiles).
plat=$([ "$(uname -s)" = Darwin ] && echo macosx || echo linux)
for tool in maked gmake; do
  cp -R "$root/benchmarks/lua_test/lua-5.4.9" "$work/lua-$tool"
  "$gmake" -C "$work/lua-$tool/src" clean >/dev/null
done
echo "==> lua-5.4.9 ($plat)"
hyperfine --runs 3 --export-json "$work/lua-cold.json" -N \
  --prepare "$gmake -C $work/lua-maked/src clean" \
  --prepare "$gmake -C $work/lua-gmake/src clean" \
  -n maked "$maked -C $work/lua-maked -j8 $plat" \
  -n gmake "$gmake -C $work/lua-gmake -j8 $plat" | grep -E "^ *Time|faster" || true
"$work/lua-maked/src/lua" -e 'assert(6*7==42)'
hyperfine --warmup 3 --runs 20 --export-json "$work/lua-null.json" -N \
  -n maked "$maked -C $work/lua-maked -j8 $plat" \
  -n gmake "$gmake -C $work/lua-gmake -j8 $plat" | grep -E "^ *Time|faster" || true
results+=("{\"scenario\":\"lua-5.4.9\",\"cold\":$(cat "$work/lua-cold.json"),\"null\":$(cat "$work/lua-null.json"),\"maked_null_peak_rss_bytes\":null}")

{
  printf '{"host":"%s","os":"%s","gmake":"%s","ninja":"%s","maked":"%s","scenarios":[' \
    "$(uname -m) $(sysctl -n machdep.cpu.brand_string 2>/dev/null || nproc)" "$(uname -sr)" "$("$gmake" --version | head -1)" "$("$ninja" --version)" "$("$maked" --version | head -1)"
  (IFS=,; printf '%s' "${results[*]}")
  printf ']}\n'
} > "$out"
echo "wrote $out"
