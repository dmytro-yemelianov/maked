#!/usr/bin/env python3
"""
Expression fuzzer: random GNU make expressions, printed with $(info ...) by
maked and by GNU make, compared line by line.

scripts/coverage.sh showed that nothing compared maked's function library,
substitution references and conditionals with GNU make (the parser was 55%
covered by GNU-differential checks; eval_function 31%). This generates
nested calls of the text, file-name and control functions over word lists
with awkward spacing, dots, slashes and `%`, plus substitution references,
`foreach`/`call`/`if`, wildcards over a small tree, and conditionals.

Usage: expr_fuzz.py [N] [--show K]   (N cases of 25 expressions each)
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


def clean_env(env):
    """A minimal environment, keeping the coverage profile path if set."""
    if "LLVM_PROFILE_FILE" in os.environ:
        env["LLVM_PROFILE_FILE"] = os.environ["LLVM_PROFILE_FILE"]
    return env

ATOMS = ["a", "b", "a.c", "b.o", "src/x.c", "src/y.h", "lib/z.a", "./m.c", "d/e/f.cc",
         "x%y", "%", ".hidden", "a.b.c", "/abs/p.c", "foo", "foo.c", "bar.c", "-flag",
         "k=v", "..", "dir/", "UP", "a.c.o", "tests/t1.sh"]
TREE = ["src/x.c", "src/y.h", "src/z.c", "lib/z.a", "d/e/f.cc", "top.c", "top.h", ".hidden.c"]
PATTERNS = ["%.c", "%.o", "src/%", "%", "a%", "%.h", "%c", "d/%/f.cc", "x%y", "%.c.o"]
TEXT_FUNS = ["subst", "patsubst", "strip", "findstring", "filter", "filter-out", "sort",
             "word", "wordlist", "words", "firstword", "lastword", "dir", "notdir", "suffix",
             "basename", "addsuffix", "addprefix", "join", "if", "or", "and", "foreach",
             "call", "value", "origin", "flavor", "abspath", "wildcard", "subref", "shell",
             "error-free-eval"]


def words(rng):
    n = rng.randint(0, 5)
    sep = lambda: rng.choice([" ", " ", "  ", "\t", " \t "])
    body = sep().join(rng.choice(ATOMS) for _ in range(n))
    return rng.choice(["", " ", ""]) + body + rng.choice(["", " ", ""])


def gen(rng, depth, vars_):
    """An expression producing text."""
    if depth <= 0 or rng.random() < 0.25:
        r = rng.random()
        if r < 0.45:
            return words(rng)
        if r < 0.85:
            return f"$({rng.choice(vars_)})"
        return f"${{{rng.choice(vars_)}}}"
    f = rng.choice(TEXT_FUNS)
    g = lambda: gen(rng, depth - 1, vars_)
    pat = lambda: rng.choice(PATTERNS)
    num = lambda: str(rng.choice([1, 1, 2, 3, 5]))
    if f == "subst":
        return f"$(subst {rng.choice(['.c', 'a', '/', ' ', '%', 'x'])},{rng.choice(['.o', '', 'Z', '%'])},{g()})"
    if f == "patsubst":
        return f"$(patsubst {pat()},{rng.choice(['%.o', 'obj/%', '%', 'X', '%.%'])},{g()})"
    if f in ("filter", "filter-out"):
        return f"$({f} {pat()} {rng.choice(['', pat()])},{g()})"
    if f == "findstring":
        return f"$(findstring {rng.choice(['a', '.c', 'src', ' ', 'zz'])},{g()})"
    if f == "word":
        return f"$(word {num()},{g()})"
    if f == "wordlist":
        return f"$(wordlist {num()},{num()},{g()})"
    if f in ("addsuffix", "addprefix"):
        return f"$({f} {rng.choice(['.o', 'p_', 'd/', ''])},{g()})"
    if f == "join":
        return f"$(join {g()},{g()})"
    if f == "if":
        return f"$(if {g()},{g()}{rng.choice(['', ',' + g()])})"
    if f in ("or", "and"):
        return f"$({f} {g()},{g()})"
    if f == "foreach":
        return f"$(foreach w,{g()},{rng.choice(['[$(w)]', '$(w).x', '$(notdir $(w))', '$(w)'])})"
    if f == "call":
        return f"$(call FN,{g()},{g()})"
    if f == "value":
        return f"$(value {rng.choice(vars_ + ['FN', 'NOPE'])})"
    if f == "origin":
        return f"$(origin {rng.choice(vars_ + ['FN', 'NOPE', 'CC', 'MAKE', 'PATH'])})"
    if f == "flavor":
        return f"$(flavor {rng.choice(vars_ + ['FN', 'NOPE'])})"
    if f == "abspath":
        return f"$(abspath {g()})"
    if f == "wildcard":
        return f"$(wildcard {rng.choice(['*.c', 'src/*', '*/*.c', 'src/?.c', 'nothere*', '*.h top.c', 'd/*/*', '.*.c'])})"
    if f == "subref":
        v = rng.choice(vars_)
        return rng.choice([f"$({v}:.c=.o)", f"$({v}:%.c=%.o)", f"$({v}:c=)", f"${{{v}:.o=.c}}", f"$({v}:%=p_%)"])
    if f == "shell":
        # Not a failing command: GNU make 4.3 (on CI) lets the output of a
        # command that exits 127 through to stdout and returns nothing; 4.4
        # and maked return it.
        return f"$(shell echo {rng.choice(['hi', 'a  b', '$$HOME_UNSET', 'x;echo y'])})"
    if f == "error-free-eval":
        return f"$(eval EV := {g()})$(EV)"
    return f"$({f} {g()})"  # one-argument functions


def conditional(rng, vars_, i):
    v = rng.choice(vars_)
    form = rng.choice(["ifeq", "ifneq", "ifdef", "ifndef"])
    if form in ("ifdef", "ifndef"):
        head = f"{form} {rng.choice(vars_ + ['NOPE', 'EMPTY'])}"
    else:
        a, b = f"$({v})", rng.choice(["", "a", "$(strip $(" + v + "))", "a.c"])
        head = rng.choice([f"{form} ({a},{b})", f"{form} '{a}' '{b}'", f'{form} "{a}" "{b}"',
                           f"{form} ({a}, {b})", f"{form} ( {a},{b} )"])
    return [head, f"$(info C{i}=then)", "else", f"$(info C{i}=else)", "endif"]


def case(seed, n_expr=25):
    rng = random.Random(seed)
    vars_ = ["A", "B", "C", "EMPTY", "SP"]
    lines = [f"A := {words(rng)}", f"B = {words(rng)} $(A)", f"C := {words(rng)}",
             "EMPTY :=", "SP := $(EMPTY) $(EMPTY)",
             "FN = <$(1)|$(2)|$(words $(1))>"]
    for i in range(n_expr):
        if rng.random() < 0.15:
            lines += conditional(rng, vars_, i)
        else:
            lines.append(f"$(info E{i}=[{gen(rng, rng.randint(1, 4), vars_)}])")
    lines += ["all: ; @:", ""]
    return "\n".join(lines)


def run(binary, d):
    r = subprocess.run([binary, "-s", "--no-print-directory", "all"], cwd=d, capture_output=True,
                       text=True, timeout=30, env=clean_env({"PATH": "/usr/bin:/bin", "HOME": "/nonexistent"}))
    return r.returncode, r.stdout.splitlines()


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    n = int(args[0]) if args else 200
    show = 3
    if "--show" in sys.argv:
        show = int(sys.argv[sys.argv.index("--show") + 1])
    bad_cases = 0
    bad_exprs = 0
    shown = 0
    with tempfile.TemporaryDirectory(prefix="maked_expr_") as tmp:
        d = Path(tmp)
        for f in TREE:
            (d / f).parent.mkdir(parents=True, exist_ok=True)
            (d / f).write_text("")
        for seed in range(1, n + 1):
            mf = case(seed)
            (d / "Makefile").write_text(mf)
            g = run(GMAKE, d)
            m = run(str(MAKED), d)
            if g == m:
                continue
            bad_cases += 1
            exprs = {l.split("$(info ", 1)[1].split("=", 1)[0]: l
                     for l in mf.splitlines() if l.startswith("$(info E")}
            gl, ml = dict(_split(g[1])), dict(_split(m[1]))
            diff = [k for k in sorted(set(gl) | set(ml), key=_key) if gl.get(k) != ml.get(k)]
            bad_exprs += len(diff)
            if shown < show:
                shown += 1
                print(f"  [-] seed {seed}: exit {m[0]} vs GNU {g[0]}")
                for k in diff[:4]:
                    print(f"      {exprs.get(k, k)}")
                    print(f"        GNU make: {gl.get(k)!r}")
                    print(f"        maked:    {ml.get(k)!r}")
    print(f"Expressions: {n - bad_cases}/{n} cases agree with GNU make "
          f"({bad_exprs} differing expressions)")
    sys.exit(1 if bad_cases else 0)


def _split(lines):
    for l in lines:
        if "=" in l and l[:1] in "EC":
            k, v = l.split("=", 1)
            yield k, v


def _key(k):
    return (k[0], int(k[1:]) if k[1:].isdigit() else 0)


if __name__ == "__main__":
    main()
