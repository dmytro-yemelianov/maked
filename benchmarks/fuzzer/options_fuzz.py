#!/usr/bin/env python3
"""
Options fuzzer: small makefiles with failing recipes, `-`/`@`/`+` prefixes,
recursive $(MAKE) calls and environment variables, run with random
combinations of -n -k -i -B -s -e -t -j and several goals. Compares stdout,
the exit status and which targets GNU make reports as failed.

scripts/coverage.sh showed main.rs (the option handling) and the executor's
error paths were the largest parts of maked nothing compared with GNU make.

Usage: options_fuzz.py [N] [--show K]
"""
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = Path(os.environ.get("MAKED_BIN", ROOT / "rust_make/target/release/maked"))
GMAKE = shutil.which("gmake") or shutil.which("make")


def clean_env(env):
    if "LLVM_PROFILE_FILE" in os.environ:
        env["LLVM_PROFILE_FILE"] = os.environ["LLVM_PROFILE_FILE"]
    return env


def case(rng):
    n = rng.randint(3, 7)
    names = [f"t{i}" for i in range(n)]
    lines = ["EV ?= file", "X = x", ""]
    phony = []
    for i, t in enumerate(names):
        deps = rng.sample(names[i + 1:], min(len(names) - i - 1, rng.randint(0, 2)))
        lines.append(f"{t}: {' '.join(deps)}")
        for _ in range(rng.randint(0, 3)):
            prefix = rng.choice(["", "", "@", "-", "@-", "+", "-@"])
            cmd = rng.choice([f"echo {t} ran", "false", f"echo {t} EV=$(EV) X=$(X)",
                              f"touch {t}", "exit 3", f"echo {t} $$EV", ":",
                              f"$(MAKE) -s --no-print-directory -f sub.mk sub"])
            lines.append(f"\t{prefix}{cmd}")
        if rng.random() < 0.4:
            phony.append(t)
    if phony:
        lines.insert(0, ".PHONY: " + " ".join(phony))
    # Flags only: GNU make 4.4 also lists command-line variables in a
    # sub-make's $(MAKEFLAGS), 4.3 (on CI) does not.
    files = {"sub.mk": "sub:\n\t@echo sub X=$(X) EV=$(EV) MAKEFLAGS=$(filter-out --% X=% EV=%,$(MAKEFLAGS))\n"}
    for t in names:
        if rng.random() < 0.3:
            files[t] = ""
    flags = [f for f in ["-n", "-k", "-i", "-B", "-s", "-e", "-t"] if rng.random() < 0.25]
    if rng.random() < 0.2:
        flags.append("-j4")
    goals = rng.sample(names, rng.randint(1, 2))
    cli = rng.choice([[], ["X=cli"], ["EV=cli"]])
    env = {"EV": "env"} if rng.random() < 0.5 else {}
    return "\n".join(lines) + "\n", files, flags + cli + goals, env


def run(binary, d, args, env):
    e = clean_env({"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", **env})
    r = subprocess.run([binary, "--no-print-directory", *args], cwd=d, capture_output=True,
                       text=True, timeout=30, env=e)
    name = Path(binary).name
    norm = lambda s: re.sub(rf"(^|\s){re.escape(name)}(\[\d+\])?:", r"\1MAKE:", s)
    out = [norm(l).replace(binary, "MAKE") for l in r.stdout.splitlines()]
    failed = sorted(set(re.findall(r"\[(?:[^\]:]*:\d+: )?([^\]]+)\] Error", r.stderr)))
    # GNU make 4.4 prints "touch X" twice under -t when a recipe mixes `+`
    # or $(MAKE) lines with others (notice_finished_file runs twice for the
    # file); one line is compared.
    seen_touch = set()
    out = [l for l in out if not (l.startswith("touch ") and (l in seen_touch or seen_touch.add(l)))]
    if "-j4" in args:
        out = sorted(out)
    # .maked_log is maked's own duration history (documented in the README).
    files = sorted(p.name for p in d.iterdir() if p.name != ".maked_log")
    return r.returncode, out, failed, files


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    n = int(args[0]) if args else 300
    show = int(sys.argv[sys.argv.index("--show") + 1]) if "--show" in sys.argv else 3
    bad = shown = 0
    for seed in range(1, n + 1):
        rng = random.Random(seed)
        mf, files, margs, env = case(rng)
        res = {}
        stamp = int(time.time()) - 100
        with tempfile.TemporaryDirectory(prefix="maked_opt_") as tmp:
            for name, binary in (("gmake", GMAKE), ("maked", str(MAKED))):
                d = Path(tmp) / name
                d.mkdir()
                for f, body in files.items():
                    (d / f).write_text(body)
                    # The same mtime in both trees: writing them in turn can
                    # straddle a filesystem clock tick in one tree only.
                    os.utime(d / f, (stamp, stamp))
                (d / "Makefile").write_text(mf)
                res[name] = run(binary, d, margs, env)
        if res["gmake"] == res["maked"]:
            continue
        # With -j and a failure (no -k), which other jobs had started is a
        # matter of timing, in GNU make too; only the exit status counts.
        if "-j4" in margs and "-k" not in margs and res["gmake"][0] != 0 \
                and res["gmake"][0] == res["maked"][0]:
            continue
        bad += 1
        if shown < show:
            shown += 1
            g, m = res["gmake"], res["maked"]
            print(f"  [-] seed {seed}: make {' '.join(margs)}  env {env}")
            print(f"      exit   GNU {g[0]}  maked {m[0]};  failed GNU {g[2]}  maked {m[2]}")
            if g[1] != m[1]:
                print(f"      GNU out:   {g[1]}")
                print(f"      maked out: {m[1]}")
            if g[3] != m[3]:
                print(f"      files GNU {g[3]}  maked {m[3]}")
            if show <= 3:
                print("      " + mf.replace("\n", "\n      "))
    print(f"Options: {n - bad}/{n} runs agree with GNU make")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
