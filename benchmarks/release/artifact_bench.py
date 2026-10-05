#!/usr/bin/env python3
"""
Benchmark release binaries against GNU make before they are published.

v0.2.1 shipped a Linux binary (musl) that took 1.33 s on git's null build
against GNU make's 0.33 s, while the same code linked with glibc took 0.48 s.
Every test passed, because tests run the binary cargo builds, not the one
that ships. This runs the shipped archives' binaries on a generated
git-sized makefile (parsing, `+=`, `$(eval)`, pattern rules, many
prerequisites) and fails when one is more than --limit times slower than
GNU make on the null build.

Usage: artifact_bench.py [--limit X] [--warn-only GLOB] BINARY...
"""
import argparse
import fnmatch
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

GMAKE = shutil.which("gmake") or shutil.which("make")


def write_tree(d: Path, n=1500):
    srcs = [f"src/m{i}.c" for i in range(n)]
    hdrs = [f"inc/h{i}.h" for i in range(60)]
    for f in srcs + hdrs:
        p = d / f
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("")
    lines = ["OBJS :="]
    lines += [f"OBJS += obj/m{i}.o" for i in range(n)]
    lines += [f"FLAGS_{i} := -DX{i} $(if $(filter %{i % 7},{i}),-O2,-O0)" for i in range(0, n, 3)]
    lines += ["define DEP", "obj/$(1).o: " + " ".join(hdrs[:40]), "endef",
              f"$(foreach m,$(patsubst src/%.c,%,$(wildcard src/*.c)),$(eval $(call DEP,$(m))))",
              "all: $(OBJS)", "\t@:",
              "obj/%.o: src/%.c", "\t@mkdir -p obj && touch $@", ""]
    (d / "Makefile").write_text("\n".join(lines))


def best(cmd, d, runs=5):
    t_best = None
    for _ in range(runs):
        t = time.perf_counter()
        r = subprocess.run(cmd, cwd=d, capture_output=True, text=True)
        e = time.perf_counter() - t
        if r.returncode != 0:
            raise RuntimeError(f"{cmd[0]} failed: {r.stderr[-300:]}")
        t_best = e if t_best is None else min(t_best, e)
    return t_best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=float, default=2.0)
    ap.add_argument("--warn-only", action="append", default=[],
                    help="binaries matching this glob only warn (e.g. the musl build)")
    ap.add_argument("binaries", nargs="+")
    a = ap.parse_args()
    failed = False
    with tempfile.TemporaryDirectory(prefix="maked_artifact_") as tmp:
        d = Path(tmp)
        write_tree(d)
        subprocess.run([GMAKE, "-j8", "all"], cwd=d, capture_output=True, check=True)
        g = best([GMAKE, "-j8", "all"], d)
        print(f"  GNU make null build: {g * 1000:.0f} ms")
        for b in a.binaries:
            t = best([str(Path(b).resolve()), "-j8", "all"], d)
            ratio = t / g
            warn = any(fnmatch.fnmatch(b, w) for w in a.warn_only)
            ok = ratio <= a.limit
            mark = "+" if ok else ("!" if warn else "-")
            print(f"  [{mark}] {b}: {t * 1000:.0f} ms, {ratio:.2f}x GNU make")
            failed |= not ok and not warn
    print(f"Artifacts: {'within' if not failed else 'over'} {a.limit}x GNU make on a null build")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
