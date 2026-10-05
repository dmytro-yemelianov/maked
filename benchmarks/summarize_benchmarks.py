#!/usr/bin/env python3
import json

files = [
    ("Deep Linear DAG (200 targets)", "benchmarks/results_linear.json"),
    ("Wide Fan-Out DAG (500 targets)", "benchmarks/results_wide.json"),
    ("No-Op Up-to-Date Check (500 targets)", "benchmarks/results_noop.json"),
    ("Parallel Build Scaling (32 tasks)", "benchmarks/results_parallel.json"),
]

for title, fpath in files:
    print(f"\n### {title}")
    try:
        with open(fpath) as f:
            data = json.load(f)
        print("| Implementation / Command | Mean Latency (ms) | StdDev (ms) | Min (ms) | Max (ms) | User CPU (ms) | Sys CPU (ms) | Speedup vs GNU Make 3.81 |")
        print("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |")
        base_mean = None
        for r in data["results"]:
            if "GNU Make 3.81" in r["command"]:
                base_mean = r["mean"]
                break
        for r in data["results"]:
            mean_ms = r["mean"] * 1000
            std_ms = r["stddev"] * 1000
            min_ms = r["min"] * 1000
            max_ms = r["max"] * 1000
            user_ms = r["user"] * 1000
            sys_ms = r["system"] * 1000
            if base_mean:
                speedup = f"{base_mean / r['mean']:.2f}x"
            else:
                speedup = "-"
            print(f"| **{r['command']}** | {mean_ms:.2f} | ±{std_ms:.2f} | {min_ms:.2f} | {max_ms:.2f} | {user_ms:.2f} | {sys_ms:.2f} | {speedup} |")
    except Exception as e:
        print(f"Error reading {fpath}: {e}")
