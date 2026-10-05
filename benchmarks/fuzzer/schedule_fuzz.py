#!/usr/bin/env python3
"""
Schedule fuzzer: checks maked's real -jN schedules against the Lean model.

For random DAGs whose recipes sleep for known times, it runs
`maked -jM --trace` and gives the recorded schedule (start, duration and
prerequisites of every job) to `lean_make --schedule`. That command uses the
definitions in lean_make/LeanMake/Scheduling.lean. Every schedule must be:

1. valid in the model: prerequisites finish first and at most M jobs run at
   once (`checkValid`, proved sound by `checkValid_sound`);
2. within Graham's greedy bound in its tight form, M * C <= W + (M-1) * L
   (`greedy_makespan_bound_tight`), plus a small allowance for dispatch
   latency.
   The model has zero latency between a job becoming ready and starting;
   a real executor does not.

It also feeds the checker schedules that break each rule, so a checker that
accepts everything fails the run.
"""
import os
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAKED = Path(os.environ.get("MAKED_BIN", ROOT / "rust_make/target/release/maked"))
LEAN = ROOT / "lean_make/.lake/build/bin/lean_make"

# Dispatch allowance per job, in microseconds: the time between a job's
# last prerequisite finishing and the job starting, which the model treats
# as zero. Generous for CI machines under load.
LATENCY_PER_JOB_US = 3000


def lean_check(lines):
    with tempfile.NamedTemporaryFile("w", suffix=".sched", delete=False) as f:
        f.write("\n".join(lines) + "\n")
        path = f.name
    out = subprocess.run([str(LEAN), "--schedule", path], capture_output=True, text=True, check=True)
    Path(path).unlink()
    res = {}
    for line in out.stdout.splitlines():
        key, _, val = line.partition(" ")
        res[key] = val.strip()
    return res


def self_test():
    ok = lean_check(["SLOTS 2", "JOB a 0 10", "JOB b 0 10", "JOB c 10 10 a"])
    assert ok["VALID_PREC"] == "true" and ok["VALID_CAP"] == "true", ok
    over = lean_check(["SLOTS 1", "JOB a 0 10", "JOB b 5 10"])
    assert over["VALID_CAP"] == "false", f"checker missed a capacity violation: {over}"
    early = lean_check(["SLOTS 2", "JOB a 0 10", "JOB c 5 10 a"])
    assert early["VALID_PREC"] == "false", f"checker missed a precedence violation: {early}"
    missing = lean_check(["SLOTS 2", "JOB c 5 10 ghost"])
    assert missing["VALID_PREC"] == "false", f"checker accepted an unknown prerequisite: {missing}"


def generate(rng, workdir):
    n = rng.randint(6, 18)
    jobs = []
    for i in range(n):
        names = [j[0] for j in jobs]
        preds = rng.sample(names, k=min(len(names), rng.randint(0, 3)))
        jobs.append((f"j{i}", preds, rng.randint(10, 60)))
    sinks = {name for name, _, _ in jobs} - {p for _, preds, _ in jobs for p in preds}
    lines = [".PHONY: all", f"all: {' '.join(sorted(sinks))}", ""]
    for name, preds, ms in jobs:
        lines.append(f"{name}: {' '.join(preds)}")
        lines.append(f"\t@sleep {ms / 1000:.3f} && touch $@")
        lines.append("")
    (workdir / "Makefile").write_text("\n".join(lines))
    return {name: preds for name, preds, _ in jobs}


def run_one(seed):
    rng = random.Random(seed)
    slots = rng.choice([2, 3, 4])
    with tempfile.TemporaryDirectory(prefix="maked_sched_") as tmp:
        work = Path(tmp)
        preds = generate(rng, work)
        trace = work / "trace.json"
        r = subprocess.run([str(MAKED), "-C", str(work), f"-j{slots}", f"--trace={trace}", "all"],
                           capture_output=True, text=True)
        if r.returncode != 0:
            return f"maked failed: {r.stderr.strip()}"
        events = [e for e in json.loads(trace.read_text())["traceEvents"]
                  if e.get("ph") == "X" and e.get("cat") == "rule" and e["name"] in preds]
        if len(events) != len(preds):
            return f"trace has {len(events)} of {len(preds)} jobs"
        t0 = min(e["ts"] for e in events)
        lines = [f"SLOTS {slots}"]
        for e in events:
            ps = " ".join(preds[e["name"]])
            lines.append(f"JOB {e['name']} {e['ts'] - t0} {max(1, e['dur'])} {ps}".rstrip())
        res = lean_check(lines)
        if res["VALID_PREC"] != "true":
            return f"-j{slots}: precedence violated\n" + "\n".join(lines)
        if res["VALID_CAP"] != "true":
            return f"-j{slots}: more than {slots} jobs ran at once\n" + "\n".join(lines)
        m, w, l, c = slots, int(res["WORK"]), int(res["CRIT"]), int(res["MAKESPAN"])
        allowance = LATENCY_PER_JOB_US * len(preds)
        # Tight form, greedy_makespan_bound_tight: m*C <= W + (m-1)*L.
        if m * c > w + (m - 1) * l + m * allowance:
            return (f"-j{slots}: greedy bound exceeded: makespan {c} us > "
                    f"(W + (m-1)L)/m = {(w + (m - 1) * l) / m:.0f} us (+{allowance} us allowance)")
        return (c, (w + (m - 1) * l) / m, max(l, -(-w // m)))


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 25
    self_test()
    print("[*] Lean checker self-test: rejects overlap, early start, unknown prerequisite")
    failures, ratios, gaps = [], [], []
    for seed in range(1, n + 1):
        res = run_one(seed)
        if isinstance(res, str):
            failures.append((seed, res))
            print(f"  [-] seed {seed}: {res}")
        else:
            c, graham, lower = res
            ratios.append(c / graham)
            gaps.append(c / lower)
    print(f"Schedule check: {n - len(failures)}/{n} schedules valid and within the greedy bound")
    if ratios:
        print(f"    makespan / ((W + (m-1)L)/m): max {max(ratios):.2f}, mean {sum(ratios) / len(ratios):.2f}")
        print(f"    makespan / lower bound: max {max(gaps):.2f}, mean {sum(gaps) / len(gaps):.2f}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
