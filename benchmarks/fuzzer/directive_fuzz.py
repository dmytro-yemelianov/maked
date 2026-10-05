#!/usr/bin/env python3
"""
Directive fuzzer: random makefiles built from the parts of the language
scripts/coverage.sh showed nothing compared with GNU make: define/endef,
`else if` chains, target- and pattern-specific variables (and their
inheritance by prerequisites), `+=`/`?=`/`override` with command-line
variables, export/unexport, vpath and VPATH, old-style suffix rules,
include, and errors. Recipes and $(info) print what they see; stdout and
the exit status are compared with GNU make's.

Usage: directive_fuzz.py [N] [--show K]
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
GMAKE = shutil.which("gmake") or shutil.which("make")
VALS = ["a", "b c", "x.c", "$(A)", "$(B)", "1 2 3", "", " sp ", "%", "p/q"]


def clean_env(env):
    if "LLVM_PROFILE_FILE" in os.environ:
        env["LLVM_PROFILE_FILE"] = os.environ["LLVM_PROFILE_FILE"]
    return env


def val(rng):
    return rng.choice(VALS)


def snippet(rng, i, files):
    """One feature, with a goal `g{i}` whose recipe prints what it sees."""
    k = rng.choice(["define", "elseif", "tvars", "pvars", "append", "export", "vpath",
                    "suffix", "include", "override", "chain", "defeval"])
    g = f"g{i}"
    L = []
    if k == "define":
        op = rng.choice(["", " =", " :=", " +=", " ?="])
        L += [f"define D{i}{op}", f"first {val(rng)}", f"  second $(A)", "endef",
              f"{g}: ; @echo '{g} [$(subst $(NL),|,$(D{i}))] $(words $(D{i}))'"]
    elif k == "elseif":
        conds = ["ifeq ($(A),a)", "ifneq ($(B),)", "ifdef C", "ifndef NOPE", 'ifeq "$(A)" "b c"']
        L += [rng.choice(conds), f"R{i} := one", "else " + rng.choice(conds)[2:].join(["if", ""]) if False else "else " + rng.choice(conds),
              f"R{i} := two"]
        if rng.random() < 0.5:
            L += ["else " + rng.choice(conds), f"R{i} := three"]
        L += ["else", f"R{i} := none", "endif", f"{g}: ; @echo '{g} $(R{i})'"]
    elif k == "tvars":
        op = rng.choice([":=", "=", "+=", "?="])
        L += [f"{g}: V{i} {op} {val(rng)}", f"{g}: {g}dep", f"\t@echo '{g} [$(V{i})]'",
              f"{g}dep: V{i} {rng.choice([':=', '+='])} dep", f"{g}dep:", f"\t@echo '{g}dep [$(V{i})]'"]
        if rng.random() < 0.5:
            L.insert(0, f"V{i} = {val(rng)}")
    elif k == "pvars":
        L += [f"%.p{i}: W{i} := pat {val(rng)}", f"{g}: x.p{i}", f"\t@echo '{g} [$(W{i})]'",
              f"x.p{i}:", f"\t@echo 'x.p{i} [$(W{i})]'"]
    elif k == "append":
        L += [f"S{i} {rng.choice(['=', ':='])} {val(rng)}", f"S{i} += {val(rng)}",
              f"S{i} += $(LATE{i})", f"LATE{i} = late", f"S{i} ?= ignored",
              f"{g}: ; @echo '{g} [$(S{i})]'"]
    elif k == "export":
        form = rng.choice([f"export E{i} = {val(rng)}", f"E{i} = v\nexport E{i}",
                           f"export E{i} := x\nunexport E{i}", f"E{i} = y"])
        L += form.split("\n") + [f"{g}: ; @echo '{g} [$$E{i}]'"]
    elif k == "vpath":
        files.update({f"vp{i}/f{i}.c": "", f"vq{i}/h{i}.h": ""})
        L += [rng.choice([f"vpath %.c vp{i}", f"VPATH = vp{i}:vq{i}", f"vpath %.c vp{i}\nvpath %.h vq{i}"])]
        L = "\n".join(L).split("\n")
        L += [f"{g}: f{i}.c h{i}.h", f"\t@echo '{g} [$<] [$^]'"] if "vq" in L[-1] or "VPATH" in L[-1] \
            else [f"{g}: f{i}.c", f"\t@echo '{g} [$<]'"]
    elif k == "suffix":
        files[f"s{i}.x"] = ""
        L += [".SUFFIXES: .x .y", f".x.y:", f"\t@echo 'suffix $< -> $@ [$*]'",
              f"{g}: s{i}.y", f"\t@echo '{g} [$^]'"]
    elif k == "include":
        files[f"inc{i}.mk"] = f"I{i} := included {val(rng)}\n"
        L += [rng.choice([f"include inc{i}.mk", f"-include inc{i}.mk nothere{i}.mk",
                          f"include $(wildcard inc{i}*.mk)", f"sinclude inc{i}.mk"]),
              f"{g}: ; @echo '{g} [$(I{i})]'"]
    elif k == "override":
        L += [f"override CLI = {val(rng)}" if rng.random() < 0.5 else f"CLI = file",
              f"CLI2 ?= default", f"{g}: ; @echo '{g} [$(CLI)] [$(CLI2)] [$(origin CLI)] [$(origin CLI2)]'"]
    elif k == "chain":
        L += [f"X{i} = $(Y{i}) x", f"Y{i} = $(Z{i}) y", f"Z{i} := z", f"Z{i} += zz",
              f"{g}: ; @echo '{g} [$(X{i})] [$(value X{i})] [$(flavor X{i})] [$(flavor Z{i})]'"]
    else:  # defeval
        L += [f"define R{i}", f"{g}: ; @echo '{g} [$$(A)] [$(1)] [$$@]'", "endef",
              f"$(eval $(call R{i},{rng.choice(['arg', 'two words', ''])}))"]
    return L


def case(seed):
    rng = random.Random(seed)
    files = {}
    head = ["A := a", "B = b c", "C := $(B)", "define NL", "", "", "endef"]
    body, goals = [], []
    for i in range(rng.randint(2, 6)):
        body += snippet(rng, i, files) + [""]
        goals.append(f"g{i}")
    mf = "\n".join(head + [".PHONY: all " + " ".join(goals), "all: " + " ".join(goals), ""] + body)
    cli = rng.choice([[], ["CLI=cmd"], ["CLI=cmd", "CLI2=x"], ["A=over"]])
    return mf, files, cli


def run(binary, d, cli):
    r = subprocess.run([binary, "-s", "--no-print-directory", *cli], cwd=d, capture_output=True,
                       text=True, timeout=30, env=clean_env({"PATH": "/usr/bin:/bin", "HOME": "/nonexistent"}))
    return r.returncode, r.stdout.splitlines()


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    n = int(args[0]) if args else 200
    show = int(sys.argv[sys.argv.index("--show") + 1]) if "--show" in sys.argv else 3
    bad, shown = 0, 0
    for seed in range(1, n + 1):
        mf, files, cli = case(seed)
        res = {}
        with tempfile.TemporaryDirectory(prefix="maked_dir_") as tmp:
            for name, binary in (("gmake", GMAKE), ("maked", str(MAKED))):
                d = Path(tmp) / name
                d.mkdir()
                for f, body in files.items():
                    (d / f).parent.mkdir(parents=True, exist_ok=True)
                    (d / f).write_text(body)
                (d / "Makefile").write_text(mf)
                res[name] = run(binary, d, cli)
        if res["gmake"] == res["maked"]:
            continue
        bad += 1
        if shown < show:
            shown += 1
            g, m = res["gmake"], res["maked"]
            print(f"  [-] seed {seed} {' '.join(cli)}: exit maked {m[0]} / GNU {g[0]}")
            gs, ms = set(g[1]), set(m[1])
            for l in [l for l in g[1] if l not in ms][:4]:
                print(f"      GNU only:   {l!r}")
            for l in [l for l in m[1] if l not in gs][:4]:
                print(f"      maked only: {l!r}")
            if show <= 3:
                print("      " + mf.replace("\n", "\n      "))
    print(f"Directives: {n - bad}/{n} makefiles agree with GNU make")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
