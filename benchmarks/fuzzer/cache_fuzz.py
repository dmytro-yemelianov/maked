#!/usr/bin/env python3
"""
Cache and hash fuzzer: random DAGs whose recipes concatenate their
prerequisites (`cat $^ > $@`), so every output is a function of its inputs.

--cache (the Lean model: lean_make/LeanMake/Cache.lean):
  1. Delete every target and build again: everything must come back from the
     cache, no recipe may run, and every output must be byte-identical
     (executeWithCAS_hit_invariant, cache_soundness_deterministic).
  2. Change one leaf's content: exactly the targets GNU make rebuilds run
     again, and outputs match a fresh GNU make build.

--hash (content, not timestamps):
  3. Touch a leaf without changing it: nothing is rebuilt.
  4. Change a leaf's content: the same targets as GNU make are rebuilt.
"""
import hashlib
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = ROOT / "rust_make/target/release/maked"
GMAKE = shutil.which("gmake") or shutil.which("make")


def generate(rng, d: Path):
    leaves = [f"src{i}.txt" for i in range(rng.randint(2, 5))]
    for i, leaf in enumerate(leaves):
        (d / leaf).write_text(f"leaf {i} {rng.random()}\n")
    layer, rules, targets = leaves, [], []
    for depth in range(rng.randint(2, 4)):
        nxt = []
        for j in range(rng.randint(2, 4)):
            t = f"t{depth}_{j}.out"
            rules.append((t, rng.sample(layer, min(len(layer), rng.randint(1, 3)))))
            nxt.append(t)
        targets += nxt
        layer = nxt
    lines = [".PHONY: all", f"all: {' '.join(layer)}", ""]
    for t, deps in rules:
        lines += [f"{t}: {' '.join(deps)}", "\t@cat $^ > $@ && echo BUILD $@", ""]
    (d / "Makefile").write_text("\n".join(lines))
    return leaves, targets


def run(cmd, cwd):
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    built = sorted(l.split()[1] for l in r.stdout.splitlines() if l.startswith("BUILD "))
    return r.returncode, built, r.stdout + r.stderr


def digest(d: Path, names):
    return {n: hashlib.sha256((d / n).read_bytes()).hexdigest() for n in names if (d / n).exists()}


def bump(path: Path):
    """Make a write visibly newer than everything before it."""
    time.sleep(1.05)
    os.utime(path)


def check(seed):
    rng = random.Random(seed)
    problems = []
    with tempfile.TemporaryDirectory(prefix="maked_cache_") as tmp:
        base = Path(tmp) / "base"
        base.mkdir()
        leaves, targets = generate(rng, base)
        trees = {}
        for name in ("cache", "hash", "gnu"):
            shutil.copytree(base, Path(tmp) / name)
            trees[name] = Path(tmp) / name
        cache_dir = Path(tmp) / "cas"

        # --cache: build, wipe outputs, rebuild from the cache.
        c = trees["cache"]
        cache = [str(MAKED), "--cache", f"--cache-dir={cache_dir}", "-j4", "all"]
        rc, _, out = run(cache, c)
        if rc != 0:
            return [f"--cache build failed: {out}"]
        # Only targets reachable from `all` are built.
        targets = [t for t in targets if (c / t).exists()]
        first = digest(c, targets)
        for t in targets:
            (c / t).unlink()
        rc, built, out = run(cache, c)
        if rc != 0 or built:
            problems.append(f"--cache rebuild after wiping ran recipes {built} (rc={rc})")
        if digest(c, targets) != first:
            problems.append("--cache restored outputs differ from the original build")

        # GNU make reference, and a content change on all three trees.
        g = trees["gnu"]
        run([GMAKE, "-j4", "all"], g)
        h = trees["hash"]
        run([str(MAKED), "--hash", "-j4", "all"], h)

        leaf = rng.choice(leaves)
        bump(h / leaf)  # touch, same content
        rc, built, _ = run([str(MAKED), "--hash", "-j4", "all"], h)
        if built:
            problems.append(f"--hash rebuilt {built} after a touch with no content change")

        new_text = f"changed {rng.random()}\n"
        for tree in (c, g, h):
            time.sleep(0.01)
            (tree / leaf).write_text(new_text)
        bump(g / leaf)
        _, g_built, _ = run([GMAKE, "-j4", "all"], g)
        _, c_built, _ = run(cache, c)
        _, h_built, _ = run([str(MAKED), "--hash", "-j4", "all"], h)
        if c_built != g_built:
            problems.append(f"--cache after change rebuilt {c_built}, GNU make {g_built}")
        if h_built != g_built:
            problems.append(f"--hash after change rebuilt {h_built}, GNU make {g_built}")
        ref = digest(g, targets)
        if digest(c, targets) != ref:
            problems.append("--cache outputs after the change differ from GNU make's")
        if digest(h, targets) != ref:
            problems.append("--hash outputs after the change differ from GNU make's")
    return problems


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 10
    failures = 0
    for seed in range(1, n + 1):
        problems = check(seed)
        for p in problems:
            print(f"  [-] seed {seed}: {p}")
        failures += bool(problems)
    print(f"Cache/hash check: {n - failures}/{n} DAGs consistent with the cache model and GNU make")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
