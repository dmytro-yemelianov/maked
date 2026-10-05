#!/usr/bin/env python3
"""
Pattern-rule fuzzer, three ways: maked, GNU make, and the Lean spec
(lean_make/LeanMake/Pattern.lean, through `lean_make --pattern`).

Each case is one pattern rule, `TPAT: PREREQ...` with a recipe that prints
`$@`, `$^` and `$*`, and one target to make, often in a subdirectory. The
prerequisites the Lean spec predicts are created on disk, and so are the
ones a whole-path reading would want, so a make that matches the wrong way
still finds its files and shows the difference instead of failing. Built-in
rules are off (-r) in both makes.

maked v0.2.1 matched patterns against the whole path; the existing fuzzers
never generated a pattern rule or a subdirectory, so nothing caught it.
"""
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = Path(os.environ.get("MAKED_BIN", ROOT / "rust_make/target/release/maked"))
LEAN = ROOT / "lean_make/.lake/build/bin/lean_make"
GMAKE = shutil.which("gmake") or shutil.which("make")

WORDS = ["a", "b", "src", "lib", "obj", "x", "e", "t"]


def rand_name(rng, n=None):
    return "".join(rng.choice("abcdeostx") for _ in range(n or rng.randint(1, 3)))


def target_pattern(rng):
    pre = rng.choice(["", "", "", "e", "lib", "obj/", "s."])
    suf = rng.choice([".o", ".o", "t", ".out", ""])
    if not pre and not suf:
        suf = ".o"
    return pre, suf


def prereq_pattern(rng):
    kind = rng.random()
    if kind < 0.2:
        return rand_name(rng) + ".h"  # plain name
    pre = rng.choice(["", "", "src/", "s.", "RCS/", "a/b/"])
    suf = rng.choice([".c", ".c", ",v", ".in", ""])
    return f"{pre}%{suf}"


def lean_pattern(tpat, target, prereqs):
    out = subprocess.run([str(LEAN), "--pattern", tpat, target, *prereqs],
                         capture_output=True, text=True).stdout
    if "MATCH yes" not in out:
        return None
    lines = dict(l.split(" ", 1) if " " in l else (l, "") for l in out.splitlines())
    return lines.get("PREREQS", "").split(), lines.get("STEM", "")


def whole_path(tpat, target, prereqs):
    """What matching the whole path would ask for (to create those files too)."""
    pre, suf = tpat.split("%", 1)
    if not (target.startswith(pre) and target.endswith(suf) and len(target) >= len(pre) + len(suf)):
        return []
    stem = target[len(pre):len(target) - len(suf)]
    return [p.replace("%", stem, 1) for p in prereqs]


def run(binary, d, target):
    r = subprocess.run([binary, "-r", target], cwd=d, capture_output=True, text=True)
    lines = [l for l in r.stdout.splitlines() if l.startswith("RULE ")]
    return r.returncode == 0, lines


def check(seed):
    rng = random.Random(seed)
    pre, suf = target_pattern(rng)
    tpat = f"{pre}%{suf}"
    prereqs = [prereq_pattern(rng) for _ in range(rng.randint(1, 3))]
    depth = rng.choice([0, 1, 1, 2])
    dirs = "".join(rand_name(rng) + "/" for _ in range(depth))
    stem = rand_name(rng)
    # Mostly targets that match; sometimes the pattern's own directory is
    # put in front of the target's (obj/%.o with obj/x/y.o), sometimes noise.
    if pre.endswith("/") and rng.random() < 0.5:
        target = f"{pre}{dirs}{stem}{suf}"
    elif rng.random() < 0.9:
        target = f"{dirs}{pre}{stem}{suf}"
    else:
        target = f"{dirs}{rand_name(rng)}"

    spec = lean_pattern(tpat, target, prereqs)
    with tempfile.TemporaryDirectory(prefix="maked_pat_") as tmp:
        base = Path(tmp) / "base"
        base.mkdir()
        want = (spec[0] if spec else []) + whole_path(tpat, target, prereqs)
        for f in want:
            if f.startswith("/"):
                continue  # a whole-path stem can start with '/'
            p = base / f
            p.parent.mkdir(parents=True, exist_ok=True)
            if not p.exists():
                p.write_text("")
        (base / "Makefile").write_text(
            f"{tpat}: {' '.join(prereqs)}\n\t@echo 'RULE $@ <- $^ stem=$*'\n")
        results = {}
        for name, binary in (("maked", str(MAKED)), ("gmake", GMAKE)):
            d = Path(tmp) / name
            shutil.copytree(base, d)
            results[name] = run(binary, d, target)

    problems = []
    if results["maked"] != results["gmake"]:
        problems.append(f"maked {results['maked']} != GNU make {results['gmake']}")
    ok, lines = results["gmake"]
    if spec is None:
        if ok:
            problems.append(f"Lean: no match, GNU make built it: {lines}")
    else:
        expected = f"RULE {target} <- {' '.join(dict.fromkeys(spec[0]))} stem={spec[1]}"
        if not ok or lines != [expected]:
            problems.append(f"Lean expects [{expected}], GNU make gave {ok} {lines}")
    return tpat, prereqs, target, problems


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 300
    failures = 0
    for seed in range(1, n + 1):
        tpat, prereqs, target, problems = check(seed)
        if problems:
            failures += 1
            print(f"  [-] seed {seed}: {tpat}: {' '.join(prereqs)}  make {target}")
            for p in problems:
                print(f"        {p}")
    print(f"Pattern rules: {n - failures}/{n} cases agree (maked, GNU make, Lean spec)")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
