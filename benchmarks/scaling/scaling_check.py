#!/usr/bin/env python3
"""
Complexity guard: time maked on each makefile shape at size N and 4N.
Linear work grows about 4x; a quadratic step grows about 16x. A shape
fails when the ratio exceeds --limit (default 7).

This is a regression check, not a proof. It exists because maked v0.2.1
had a quadratic `+=` (git's generated makefile made ~11,000 appends to the
same variables) that no test noticed, and because Lean models the build's
results, not maked's running time.

Usage: scaling_check.py [--n N] [--limit X] [--only SHAPE]
"""
import argparse
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = ROOT / "rust_make/target/release/maked"


def chain(n):
    lines = ["all: t0", ""]
    for i in range(n):
        lines += [f"t{i}: t{i + 1}" if i + 1 < n else f"t{i}: seed", "\t@:", ""]
    return "\n".join(lines), ["seed"]


def wide(n):
    return "all: " + " ".join(f"f{i}" for i in range(n)) + "\n\t@:\n", [f"f{i}" for i in range(n)]


def appends(n):
    # git's cocci matrix: thousands of `+=` on a few variables.
    body = "".join(f"V{i % 4} += word{i}\n" for i in range(n))
    return body + "all:\n\t@echo $(words $(V0) $(V1) $(V2) $(V3))\n", []


def eval_rules(n):
    return ("NAMES := " + " ".join(f"n{i}" for i in range(n)) + "\n"
            "define R\n$(1).out: ; @:\nendef\n"
            "$(foreach x,$(NAMES),$(eval $(call R,$(x))))\n"
            "all: $(NAMES:%=%.out)\n\t@:\n"), []


def long_line(n):
    # One rule with n prerequisites on a single long line.
    return "all: " + " ".join(f"p{i}" for i in range(n)) + "\n\t@:\n" + \
        "".join(f"p{i}: ; @:\n" for i in range(n)), []


def patterns(n):
    # Every object goes through a pattern rule in a subdirectory.
    return ("all: " + " ".join(f"d{i % 50}/o{i}.o" for i in range(n)) + "\n\t@:\n"
            "%.o: %.c\n\t@:\n"), [f"d{i % 50}/o{i}.c" for i in range(n)]


def variables(n):
    body = "".join(f"X{i} = $(X{i - 1}) v{i}\n" if i else "X0 = v0\n" for i in range(0, n, 1))
    return body + f"all:\n\t@echo $(words $(X{min(n - 1, 50)}))\n", []


SHAPES = {
    "chain": (chain, 2000),
    "wide": (wide, 5000),
    "appends": (appends, 25000),
    "eval_rules": (eval_rules, 2000),
    "long_line": (long_line, 4000),
    "patterns": (patterns, 2000),
    "variables": (variables, 5000),
}


def best_time(d: Path, runs=3):
    best = None
    for _ in range(runs):
        t = time.perf_counter()
        r = subprocess.run([str(MAKED), "-j1", "all"], cwd=d, capture_output=True, text=True)
        e = time.perf_counter() - t
        if r.returncode != 0:
            raise RuntimeError(r.stderr[-400:])
        best = e if best is None else min(best, e)
    return best


def measure(gen, n):
    with tempfile.TemporaryDirectory(prefix="maked_scale_") as tmp:
        d = Path(tmp)
        text, files = gen(n)
        (d / "Makefile").write_text(text)
        for f in files:
            p = d / f
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text("")
        subprocess.run([str(MAKED), "all"], cwd=d, capture_output=True)  # build once
        return best_time(d)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scale", type=float, default=1.0, help="multiply every N")
    ap.add_argument("--limit", type=float, default=7.0)
    ap.add_argument("--only")
    a = ap.parse_args()
    failed = []
    for name, (gen, n) in SHAPES.items():
        if a.only and name != a.only:
            continue
        n = max(10, int(n * a.scale))
        small, big = measure(gen, n), measure(gen, 4 * n)
        ratio = big / small
        ok = ratio <= a.limit
        print(f"  [{'+' if ok else '-'}] {name:11} N={n:>6}: {small * 1000:7.1f} ms  4N: "
              f"{big * 1000:7.1f} ms  ratio {ratio:4.1f}")
        if not ok:
            failed.append(name)
    print(f"Scaling: {'all shapes near-linear' if not failed else 'superlinear: ' + ', '.join(failed)}"
          f" (limit {a.limit}x for 4x input)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
