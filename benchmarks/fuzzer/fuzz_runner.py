#!/usr/bin/env python3
"""
3-Way Differential Oracle Suite: maked (Rust) vs GNU Make (POSIX/C) vs Lean 4 Formal Model
Tests random DAG topologies with varying depths, fan-outs, diamond dependencies,
phony targets, and incremental file modifications.
Verifies that:
1. maked and gmake make identical physical build and rebuild decisions.
2. The Lean 4 formal operational semantics model agrees 100% with maked and gmake on DAG freshness.
"""

import os
import sys
import random
import shutil
import tempfile
import subprocess
import json
import time
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[2]
MAKED_BIN = WORKSPACE / "rust_make/target/release/maked"
GMAKE_BIN = Path("/opt/homebrew/bin/gmake") if Path("/opt/homebrew/bin/gmake").exists() else Path("/usr/bin/make")
LEAN_MAKE_BIN = WORKSPACE / "lean_make/.lake/build/bin/lean_make"

class DAGGenerator:
    def __init__(self, seed: int, max_depth: int = 4, max_fanout: int = 3):
        self.rng = random.Random(seed)
        self.max_depth = max_depth
        self.max_fanout = max_fanout

    def generate_dag(self, workdir: Path):
        """Generates random source files, target rules, and Makefile"""
        num_leaves = self.rng.randint(2, 6)
        leaves = [f"src_{i}.c" for i in range(num_leaves)]
        leaf_mtimes = {}
        base_time = 1700000000 + self.rng.randint(1000, 50000)

        for i, leaf in enumerate(leaves):
            p = workdir / leaf
            p.write_text(f"// source file {leaf}\nint fn_{leaf[:5]}() {{ return {self.rng.randint(1, 100)}; }}\n")
            mtime = base_time + i * 10
            os.utime(p, (mtime, mtime))
            leaf_mtimes[leaf] = mtime

        layers = [leaves]
        rules = []

        current_layer = leaves
        depth = self.rng.randint(2, self.max_depth)
        # Rule kinds. Until v0.2.2 every generated rule had a recipe that
        # created its file, so recipe-less rules (aliases, the `FORCE:`
        # idiom) were never compared, and the Lean model got them wrong.
        kinds = ["normal"] * 6 + ["alias", "phony_alias", "phony_cmd", "existing_alias", "no_touch"]
        for d in range(depth):
            next_layer = []
            layer_size = self.rng.randint(2, 4)
            for j in range(layer_size):
                tgt = f"node_{d}_{j}.o"
                fanout = min(len(current_layer), self.rng.randint(1, self.max_fanout))
                prereqs = self.rng.sample(current_layer, fanout)
                kind = self.rng.choice(kinds)
                commands = {
                    "normal": [f'@echo "BUILD {tgt}" && touch {tgt}'],
                    "phony_cmd": [f'@echo "BUILD {tgt}"'],
                    "no_touch": [f'@echo "BUILD {tgt}"'],
                }.get(kind, [])
                if kind == "existing_alias":
                    p = workdir / tgt
                    p.write_text("")
                    mtime = base_time + self.rng.choice([-500, 5, 5000])
                    os.utime(p, (mtime, mtime))
                    leaf_mtimes[tgt] = mtime

                rules.append({
                    "target": tgt,
                    "prereqs": prereqs,
                    "commands": commands,
                    "is_phony": kind in ("phony_alias", "phony_cmd"),
                })
                next_layer.append(tgt)
            layers.append(next_layer)
            current_layer = next_layer

        # The `FORCE:` idiom: no prerequisites, no recipe, no file.
        if self.rng.random() < 0.5:
            victim = self.rng.choice(rules)
            victim["prereqs"] = victim["prereqs"] + ["FORCE"]
            rules.append({"target": "FORCE", "prereqs": [], "commands": [], "is_phony": False})

        # Top-level 'all' target
        top_prereqs = current_layer
        rules.append({
            "target": "all",
            "prereqs": top_prereqs,
            "commands": ['@echo "BUILD all"'],
            "is_phony": True
        })

        # Generate Makefile string
        phony = ["all"] + [r["target"] for r in rules if r["is_phony"]]
        lines = [
            "# Auto-generated fuzz Makefile",
            ".PHONY: " + " ".join(phony),
            "",
        ]
        for r in rules:
            prereqs_str = " ".join(r["prereqs"])
            lines.append(f"{r['target']}: {prereqs_str}")
            for cmd in r["commands"]:
                lines.append(f"\t{cmd}")
            lines.append("")

        makefile_path = workdir / "Makefile"
        makefile_path.write_text("\n".join(lines))
        return leaves, rules, leaf_mtimes

def run_make_command(cmd_args, cwd: Path):
    res = subprocess.run(
        cmd_args,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    rebuilt = []
    for line in res.stdout.splitlines():
        if line.startswith("BUILD "):
            rebuilt.append(line.split("BUILD ")[1].strip())
    return {
        "exit_code": res.returncode,
        "stdout": res.stdout,
        "stderr": res.stderr,
        "rebuilt": sorted(rebuilt)
    }

def run_lean_make(spec_path: Path):
    res = subprocess.run(
        [str(LEAN_MAKE_BIN), "--eval", str(spec_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    outcome = "unknown"
    rebuilt = []
    uptodate = []
    failed = []
    ran = []

    for line in res.stdout.splitlines():
        line = line.strip()
        if line.startswith("OUTCOME "):
            outcome = line.split("OUTCOME ")[1].strip()
        elif line.startswith("REBUILT "):
            tokens = line.split("REBUILT ")[1].strip().split()
            rebuilt = [t for t in tokens if t]
        elif line.startswith("UPTODATE "):
            tokens = line.split("UPTODATE ")[1].strip().split()
            uptodate = [t for t in tokens if t]
        elif line.startswith("RAN"):
            ran = [t for t in line[3:].split() if t]
        elif line.startswith("FAILED "):
            tokens = line.split("FAILED ")[1].strip().split()
            failed = [t for t in tokens if t]

    return {
        "exit_code": res.returncode,
        "outcome": outcome,
        "rebuilt": sorted(rebuilt),
        "uptodate": sorted(uptodate),
        "failed": sorted(failed),
        "ran": sorted(ran),
        "raw_stdout": res.stdout
    }

def generate_lean_spec(rules: list, fs_mtimes: dict, target: str, out_path: Path):
    lines = []
    for r in rules:
        is_phony = 1 if r["is_phony"] else 0
        has_cmds = 1 if len(r["commands"]) > 0 else 0
        prereqs_str = " ".join(r["prereqs"])
        lines.append(f"RULE {r['target']} {is_phony} {has_cmds} {prereqs_str}")
    for name, mtime in fs_mtimes.items():
        lines.append(f"FS {name} {int(mtime)}")
    lines.append(f"TARGET {target}")
    out_path.write_text("\n".join(lines))

def get_disk_mtimes(d: Path) -> dict:
    mtimes = {}
    for p in d.iterdir():
        if p.is_file() and p.name != "Makefile" and not p.name.startswith("spec"):
            mtimes[p.name] = int(p.stat().st_mtime)
    return mtimes

def run_differential_test(seed: int) -> dict:
    with tempfile.TemporaryDirectory(prefix="maked_3way_oracle_") as tmpdir:
        workdir = Path(tmpdir)
        gen = DAGGenerator(seed)
        leaves, rules, leaf_mtimes = gen.generate_dag(workdir)

        test_result = {
            "seed": seed,
            "passed": True,
            "discrepancies": []
        }

        # ---------------- Phase 1: Clean Initial Build (3-way) ----------------
        maked_dir = workdir / "maked_run"
        gmake_dir = workdir / "gmake_run"
        shutil.copytree(workdir, maked_dir, ignore=shutil.ignore_patterns("maked_run", "gmake_run"))
        shutil.copytree(workdir, gmake_dir, ignore=shutil.ignore_patterns("maked_run", "gmake_run"))

        spec1 = workdir / "spec1.txt"
        generate_lean_spec(rules, leaf_mtimes, "all", spec1)

        out_maked_1 = run_make_command([str(MAKED_BIN), "all"], maked_dir)
        out_gmake_1 = run_make_command([str(GMAKE_BIN), "all"], gmake_dir)
        out_lean_1 = run_lean_make(spec1)

        if out_maked_1["rebuilt"] != out_gmake_1["rebuilt"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 1: maked vs gmake Initial Build",
                "maked_rebuilt": out_maked_1["rebuilt"],
                "gmake_rebuilt": out_gmake_1["rebuilt"],
            })

        if out_maked_1["rebuilt"] != out_lean_1["ran"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 1: maked vs Lean 4 Formal Model Initial Build",
                "maked_rebuilt": out_maked_1["rebuilt"],
                "lean_ran": out_lean_1["ran"],
            })

        # ---------------- Phase 2: Re-run Parity & Idempotency (3-way) ----------------
        out_maked_2 = run_make_command([str(MAKED_BIN), "all"], maked_dir)
        out_gmake_2 = run_make_command([str(GMAKE_BIN), "all"], gmake_dir)

        maked_concrete_rebuilt = [t for t in out_maked_2["rebuilt"] if t != "all"]
        gmake_concrete_rebuilt = [t for t in out_gmake_2["rebuilt"] if t != "all"]

        if maked_concrete_rebuilt != gmake_concrete_rebuilt:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 2: maked vs gmake Re-run Parity",
                "maked_concrete_rebuilt": maked_concrete_rebuilt,
                "gmake_concrete_rebuilt": gmake_concrete_rebuilt,
            })

        # Reflect exact physical filesystem state from maked_dir into Lean model
        phase2_mtimes = get_disk_mtimes(maked_dir)
        spec2 = workdir / "spec2.txt"
        generate_lean_spec(rules, phase2_mtimes, "all", spec2)
        out_lean_2 = run_lean_make(spec2)

        lean_concrete_rebuilt = [t for t in out_lean_2["ran"] if t != "all"]
        if maked_concrete_rebuilt != lean_concrete_rebuilt:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 2: maked vs Lean 4 Idempotency",
                "maked_concrete_rebuilt": maked_concrete_rebuilt,
                "lean_concrete_rebuilt": lean_concrete_rebuilt,
            })

        # ---------------- Phase 3: Incremental Rebuild (3-way) ----------------
        mod_leaf = leaves[0]
        max_existing_mtime = max(phase2_mtimes.values())
        new_mtime = max_existing_mtime + 500

        (maked_dir / mod_leaf).write_text(f"// modified leaf\nint x = {seed};\n")
        (gmake_dir / mod_leaf).write_text(f"// modified leaf\nint x = {seed};\n")
        os.utime(maked_dir / mod_leaf, (new_mtime, new_mtime))
        os.utime(gmake_dir / mod_leaf, (new_mtime, new_mtime))

        phase3_mtimes = get_disk_mtimes(maked_dir)
        spec3 = workdir / "spec3.txt"
        generate_lean_spec(rules, phase3_mtimes, "all", spec3)

        out_maked_3 = run_make_command([str(MAKED_BIN), "all"], maked_dir)
        out_gmake_3 = run_make_command([str(GMAKE_BIN), "all"], gmake_dir)
        out_lean_3 = run_lean_make(spec3)

        if out_maked_3["rebuilt"] != out_gmake_3["rebuilt"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": f"Phase 3: maked vs gmake Incremental Rebuild on {mod_leaf}",
                "maked_rebuilt": out_maked_3["rebuilt"],
                "gmake_rebuilt": out_gmake_3["rebuilt"],
            })

        if out_maked_3["rebuilt"] != out_lean_3["ran"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": f"Phase 3: maked vs Lean 4 Incremental Rebuild on {mod_leaf}",
                "maked_rebuilt": out_maked_3["rebuilt"],
                "lean_ran": out_lean_3["ran"],
            })

        # ---------------- Phase 4: Question Mode (-q) ----------------
        non_phony_tgt = rules[0]["target"]
        q_maked = run_make_command([str(MAKED_BIN), "-q", non_phony_tgt], maked_dir)
        q_gmake = run_make_command([str(GMAKE_BIN), "-q", non_phony_tgt], gmake_dir)

        if q_maked["exit_code"] != q_gmake["exit_code"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 4: Question Mode (-q)",
                "maked_code": q_maked["exit_code"],
                "gmake_code": q_gmake["exit_code"],
            })

        # ---------------- Phase 5: Touch Mode (-t) ----------------
        # Fresh copies of the generated tree: what -t prints and which files
        # it leaves (only targets with a recipe are touched).
        touched = {}
        for name, binary in (("maked", MAKED_BIN), ("gmake", GMAKE_BIN)):
            tdir = workdir / f"touch_{name}"
            shutil.copytree(workdir, tdir, ignore=shutil.ignore_patterns(
                "maked_run", "gmake_run", "touch_*", "spec*"))
            out = run_make_command([str(binary), "-t", "all"], tdir)
            # make's own messages start with its name; compare them as MAKE.
            lines = [l.replace(f"{Path(str(binary)).name}:", "MAKE:", 1)
                     for l in out["stdout"].split("\n")]
            touched[name] = (out["exit_code"], sorted(lines),
                             sorted(p.name for p in tdir.iterdir()))
        if touched["maked"] != touched["gmake"]:
            test_result["passed"] = False
            test_result["discrepancies"].append({
                "phase": "Phase 5: Touch Mode (-t)",
                "maked": touched["maked"],
                "gmake": touched["gmake"],
            })

        return test_result

def main():
    num_iterations = int(sys.argv[1]) if len(sys.argv) > 1 else 50
    print("=" * 70)
    print("3-Way Differential Oracle Suite: maked (Rust) vs gmake vs Lean 4 Formal Model")
    print("=" * 70)
    print(f"[*] Target Binaries:")
    print(f"    - maked:     {MAKED_BIN}")
    print(f"    - gmake:     {GMAKE_BIN}")
    print(f"    - lean_make: {LEAN_MAKE_BIN}")
    print(f"[*] Executing {num_iterations} randomized DAG topologies across 4 phases each...")

    passed_count = 0
    failures = []

    start_time = time.time()
    for seed in range(1, num_iterations + 1):
        res = run_differential_test(seed)
        if res["passed"]:
            passed_count += 1
            if seed % 10 == 0 or seed == num_iterations:
                print(f"  [+] Topology {seed:2d}/{num_iterations}: 100% 3-WAY PARITY (maked == gmake == Lean 4)")
        else:
            print(f"  [-] Topology {seed:2d}/{num_iterations}: DISCREPANCY DETECTED!")
            for d in res["discrepancies"]:
                print(f"      Phase: {d['phase']}")
                for k, v in d.items():
                    if k != "phase":
                        print(f"        {k}: {v}")
            failures.append(res)

    elapsed = time.time() - start_time
    print("\n" + "=" * 70)
    print(f"3-Way Differential Results: {passed_count}/{num_iterations} passed ({passed_count/num_iterations*100:.1f}%) in {elapsed:.2f}s")
    print("=" * 70)

    summary = {
        "suite": "3-Way Differential Oracle (maked vs gmake vs Lean 4)",
        "total_topologies": num_iterations,
        "passed": passed_count,
        "failed": len(failures),
        "pass_rate_pct": (passed_count / num_iterations) * 100,
        "elapsed_seconds": elapsed,
        "failures": failures
    }

    report_path = WORKSPACE / "benchmarks/fuzzer/oracle_3way_report.json"
    report_path.write_text(json.dumps(summary, indent=2))
    print(f"[*] Comprehensive 3-way oracle report saved to {report_path}")

    if failures:
        sys.exit(1)

if __name__ == "__main__":
    main()
