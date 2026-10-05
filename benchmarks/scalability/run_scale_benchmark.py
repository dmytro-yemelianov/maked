#!/usr/bin/env python3
"""
Comprehensive Scalability & Stress Benchmark Runner:
Compares makeyd (Rust), GNU Make 4.4.1 (gmake), and Ninja
across massive DAGs (1k to 10k nodes) measuring:
  1. Null-build / Up-to-date traversal latency
  2. Parse & Dry-Run (-n) latency
  3. Peak Resident Set Size (RSS memory footprint)
  4. Parallel scaling (-j 1, -j 4, -j 8, -j 16)
Emits benchmarks/scalability/scalability_report.json
"""

import os
import sys
import subprocess
import time
import json
import shutil
import tempfile

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MAKEYD_BIN = os.path.join(ROOT_DIR, "rust_make", "target", "release", "makeyd")
GMAKE_BIN = "/opt/homebrew/bin/gmake"
NINJA_BIN = "/opt/homebrew/bin/ninja"

def measure_peak_rss_and_time(cmd, cwd):
    """
    Runs cmd under /usr/bin/time -l on macOS to measure peak RSS in bytes and real time.
    """
    time_cmd = ["/usr/bin/time", "-l"] + cmd
    start = time.perf_counter()
    proc = subprocess.run(time_cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    elapsed = time.perf_counter() - start

    peak_rss_bytes = 0
    for line in proc.stderr.splitlines():
        if "maximum resident set size" in line:
            parts = line.strip().split()
            # On macOS /usr/bin/time -l reports RSS in bytes
            peak_rss_bytes = int(parts[0])
            break

    return {
        "exit_code": proc.returncode,
        "elapsed_sec": elapsed,
        "peak_rss_kb": round(peak_rss_bytes / 1024.0, 1),
    }

def run_hyperfine(cmds, cwd, runs=10):
    """
    Runs hyperfine comparing multiple command strings.
    """
    json_path = os.path.join(cwd, "_hf_out.json")
    hf_args = ["hyperfine", "--export-json", json_path, f"--runs={runs}"]
    for name, cmd in cmds:
        hf_args.extend(["-n", name, cmd])

    proc = subprocess.run(hf_args, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if os.path.exists(json_path):
        try:
            with open(json_path) as f:
                data = json.load(f)
            os.remove(json_path)
            return data
        except Exception:
            return None
    return None

def main():
    print("=" * 70)
    print("Mega-Project Scalability & Stress Benchmark: makeyd vs gmake vs ninja")
    print("=" * 70)

    # Ensure makeyd is built in release mode
    subprocess.run(["cargo", "build", "--release", "--manifest-path", os.path.join(ROOT_DIR, "rust_make", "Cargo.toml")], check=True)

    benchmark_results = {
        "environment": {
            "os": "macOS (Apple Silicon arm64)",
            "makeyd": "v0.1.0 (Zero-Dependency Rust)",
            "gmake": "GNU Make 4.4.1",
            "ninja": "1.12.1",
        },
        "scenarios": {},
    }

    test_sizes = [
        ("Modular_1000", "modular", 1000),
        ("Diamond_2500", "diamond", 2500),
        ("Modular_5000", "modular", 5000),
        ("Modular_10000", "modular", 10000),
    ]

    for label, topo, size in test_sizes:
        print(f"\n--- Scenario: {label} ({size} nodes) ---")
        tmp_dir = tempfile.mkdtemp(prefix=f"makeyd_scale_{label}_")
        gen_script = os.path.join(ROOT_DIR, "benchmarks", "scalability", "generate_massive_dag.py")

        # 1. Generate DAG
        subprocess.run([sys.executable, gen_script, "--topology", topo, "--size", str(size), "--out", tmp_dir], check=True)

        scenario_res = {
            "topology": topo,
            "target_count": size,
            "null_build_latency": {},
            "dry_run_latency": {},
            "peak_rss": {},
            "parallel_build_scaling": {},
        }

        # 2. Benchmark Cold Build & Measure Peak RSS
        print("  [*] Performing cold initial build...")
        makeyd_cold = measure_peak_rss_and_time([MAKEYD_BIN, "-j", "8"], tmp_dir)
        scenario_res["peak_rss"]["makeyd"] = makeyd_cold["peak_rss_kb"]
        scenario_res["cold_build_time_sec"] = makeyd_cold["elapsed_sec"]

        # Clean for gmake comparison
        subprocess.run([GMAKE_BIN, "clean"], cwd=tmp_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        gmake_cold = measure_peak_rss_and_time([GMAKE_BIN, "-j", "8"], tmp_dir)
        scenario_res["peak_rss"]["gmake"] = gmake_cold["peak_rss_kb"]

        # Rebuild app to up-to-date state
        subprocess.run([MAKEYD_BIN, "-j", "8"], cwd=tmp_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        # 3. Benchmark Null-Build (Idempotency traversal)
        print("  [*] Benchmarking Null-Build traversal (Hyperfine)...")
        hf_null = run_hyperfine([
            ("makeyd", f"{MAKEYD_BIN} -j 8"),
            ("gmake", f"{GMAKE_BIN} -j 8"),
            ("ninja", f"{NINJA_BIN} -j 8"),
        ], cwd=tmp_dir, runs=15)

        if hf_null and "results" in hf_null:
            for r in hf_null["results"]:
                scenario_res["null_build_latency"][r["command"]] = {
                    "mean_ms": round(r["mean"] * 1000.0, 2),
                    "stddev_ms": round(r["stddev"] * 1000.0, 2),
                    "min_ms": round(r["min"] * 1000.0, 2),
                    "max_ms": round(r["max"] * 1000.0, 2),
                }
                print(f"      - {r['command']:<10}: {r['mean']*1000.0:6.2f} ms ± {r['stddev']*1000.0:4.2f} ms")

        # 4. Benchmark Dry-Run (-n)
        print("  [*] Benchmarking Dry-Run (-n) parse & plan speed...")
        hf_dry = run_hyperfine([
            ("makeyd -n", f"{MAKEYD_BIN} -n"),
            ("gmake -n", f"{GMAKE_BIN} -n"),
            ("ninja -n", f"{NINJA_BIN} -n"),
        ], cwd=tmp_dir, runs=10)

        if hf_dry and "results" in hf_dry:
            for r in hf_dry["results"]:
                scenario_res["dry_run_latency"][r["command"]] = {
                    "mean_ms": round(r["mean"] * 1000.0, 2),
                }

        # 5. Measure Parallel Thread Scaling on makeyd (-j 1, 4, 8, 16)
        print("  [*] Benchmarking makeyd Parallel Scaling...")
        for j in [1, 4, 8, 16]:
            subprocess.run([GMAKE_BIN, "clean"], cwd=tmp_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            t_res = measure_peak_rss_and_time([MAKEYD_BIN, f"-j{j}"], tmp_dir)
            scenario_res["parallel_build_scaling"][f"-j{j}"] = {
                "elapsed_sec": round(t_res["elapsed_sec"], 3),
                "peak_rss_kb": t_res["peak_rss_kb"],
            }
            print(f"      - makeyd -j{j:<2}: {t_res['elapsed_sec']:6.3f}s (RSS: {t_res['peak_rss_kb']/1024.0:4.1f} MB)")

        benchmark_results["scenarios"][label] = scenario_res
        shutil.rmtree(tmp_dir)

    report_path = os.path.join(ROOT_DIR, "benchmarks", "scalability", "scalability_report.json")
    with open(report_path, "w") as f:
        json.dump(benchmark_results, f, indent=2)

    print("\n" + "=" * 70)
    print(f"[+] Complete Scalability Benchmark Report saved to {report_path}")
    print("=" * 70)

if __name__ == "__main__":
    main()
