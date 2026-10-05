#!/usr/bin/env python3
"""
Remaking-makefiles fuzzer: random included makefiles with rules, built by
maked and GNU make; every recipe appends `<target> <MAKE_RESTARTS>` to a log.

Checks:
  1. The logs match GNU make's (as multisets per restart), and so do the
     goals' output and exit status.
  2. Run-once, the property `no_recipe_runs_twice` proves in
     lean_make/LeanMake/RunOnce.lean: within one invocation (one value of
     MAKE_RESTARTS) no recipe runs twice. maked v0.2.1 failed this on git's
     `GIT-VERSION-FILE: FORCE`.

Included files converge: a recipe writes fixed content, and only when the
file is missing or different, so make restarts a bounded number of times.
"""
import random
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = ROOT / "rust_make/target/release/maked"
GMAKE = shutil.which("gmake") or shutil.which("make")


def generate(rng, d: Path):
    n_inc = rng.randint(1, 3)
    srcs = [f"s{i}.txt" for i in range(rng.randint(1, 3))]
    for s in srcs:
        (d / s).write_text(s)
    lines = [".PHONY: all", "all: out", "\t@echo all V=$(V)", "",
             "out: " + " ".join(rng.sample(srcs, 1)), "\t@echo out $(MAKE_RESTARTS) >> log; touch $@", ""]
    for i in range(n_inc):
        inc = f"inc{i}.mk"
        content = f"V{i} = {rng.randint(0, 9)}\\nV += {i}"
        if rng.random() < 0.6:  # exists already, maybe with other content
            same = rng.random() < 0.5
            (d / inc).write_text((content if same else f"V{i} = old").replace("\\n", "\n") + "\n")
        deps = []
        if rng.random() < 0.5:
            deps.append("FORCE")
        if rng.random() < 0.5:
            deps.append(rng.choice(srcs))
        if i > 0 and rng.random() < 0.4:
            deps.append(f"inc{rng.randrange(i)}.mk")
        if rng.random() < 0.3:
            lines += ["out: " + inc, ""]  # a goal depends on it too
        write = rng.choice(["converge", "converge", "keep"])
        if write == "converge":
            recipe = (f"\t@echo {inc} $(MAKE_RESTARTS) >> log; "
                      f"printf '{content}\\n' > $@.tmp; "
                      f"cmp -s $@.tmp $@ && rm $@.tmp || mv $@.tmp $@")
        else:  # GIT-VERSION-FILE style: run, change nothing that exists
            recipe = (f"\t@echo {inc} $(MAKE_RESTARTS) >> log; "
                      f"test -f $@ || printf '{content}\\n' > $@")
        lines += [f"{inc}: {' '.join(deps)}", recipe, ""]
        lines.append(("-include " if rng.random() < 0.5 else "include ") + inc)
        lines.append("")
    lines += ["FORCE:", ""]
    (d / "Makefile").write_text("\n".join(lines))


def run(binary, d: Path):
    (d / "log").unlink(missing_ok=True)
    r = subprocess.run([binary, "--no-print-directory"], cwd=d, capture_output=True,
                       text=True, timeout=60, env={"PATH": "/usr/bin:/bin"})
    log = (d / "log").read_text().split("\n") if (d / "log").exists() else []
    # `name` or `name N`: N is $(MAKE_RESTARTS), empty before a restart.
    entries = [tuple((l.split() + ["0"])[:2]) for l in log if l.strip()]
    out = [l for l in r.stdout.splitlines() if l.startswith("all ")]
    return r.returncode, out, entries


def check(seed):
    rng = random.Random(seed)
    problems = []
    with tempfile.TemporaryDirectory(prefix="maked_remake_") as tmp:
        base = Path(tmp) / "base"
        base.mkdir()
        generate(rng, base)
        res = {}
        for name, binary in (("maked", str(MAKED)), ("gmake", GMAKE)):
            d = Path(tmp) / name
            shutil.copytree(base, d)
            first = run(binary, d)
            second = run(binary, d)  # a second run: the steady state
            res[name] = (first, second)
            for rc, _, entries in (first, second):
                twice = [e for e, c in Counter(entries).items() if c > 1]
                if twice:
                    problems.append(f"{name}: recipe ran twice in one invocation: {twice}")
        for i, phase in enumerate(("first run", "second run")):
            m, g = res["maked"][i], res["gmake"][i]
            if (m[0], m[1], Counter(m[2])) != (g[0], g[1], Counter(g[2])):
                problems.append(f"{phase}: maked {m} != GNU make {g}")
        mf = (base / "Makefile").read_text()
    return problems, mf


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 200
    failures = 0
    for seed in range(1, n + 1):
        problems, mf = check(seed)
        if problems:
            failures += 1
            print(f"  [-] seed {seed}:")
            for p in problems:
                print(f"        {p}")
            if failures <= 2:
                print("        " + mf.replace("\n", "\n        "))
    print(f"Remaking makefiles: {n - failures}/{n} cases agree with GNU make, no recipe ran twice")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
