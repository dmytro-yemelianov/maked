#!/usr/bin/env python3
import os
import sys

def gen_linear(n=200):
    lines = []
    lines.append(f"all: t_{n}")
    lines.append(f"\t@echo 'Build finished at t_{n}'")
    for i in range(n, 0, -1):
        dep = f"t_{i-1}" if i > 1 else "leaf.txt"
        lines.append(f"t_{i}: {dep}")
        lines.append(f"\t@echo 'Building t_{i}'")
    
    with open("benchmarks/linear/Makefile", "w") as f:
        f.write("\n".join(lines) + "\n")
    with open("benchmarks/linear/leaf.txt", "w") as f:
        f.write("leaf content\n")

def gen_wide(n=500):
    lines = []
    all_deps = " ".join([f"node_{i}.o" for i in range(n)])
    lines.append(f"all: {all_deps}")
    lines.append(f"\t@echo 'Linked all {n} nodes'")
    for i in range(n):
        lines.append(f"node_{i}.o: node_{i}.c")
        lines.append(f"\t@echo 'Compiling node_{i}'")
    
    with open("benchmarks/wide/Makefile", "w") as f:
        f.write("\n".join(lines) + "\n")
    
    # Also create the .c files
    for i in range(n):
        with open(f"benchmarks/wide/node_{i}.c", "w") as f:
            f.write(f"// source {i}\n")

def gen_parallel(n=32):
    lines = []
    all_deps = " ".join([f"task_{i}.out" for i in range(n)])
    lines.append(f"all: {all_deps}")
    lines.append(f"\t@echo 'All {n} parallel tasks completed'")
    for i in range(n):
        # A tiny sleep of 0.05 seconds simulates realistic compilation work
        lines.append(f"task_{i}.out: task_{i}.in")
        lines.append(f"\t@python3 -c 'import time; time.sleep(0.04)' && echo 'done {i}' > task_{i}.out")
    
    with open("benchmarks/parallel/Makefile", "w") as f:
        f.write("\n".join(lines) + "\n")
    
    for i in range(n):
        with open(f"benchmarks/parallel/task_{i}.in", "w") as f:
            f.write(f"input {i}\n")

if __name__ == "__main__":
    print("Generating benchmark suites...")
    gen_linear(200)
    gen_wide(500)
    gen_parallel(32)
    print("Benchmark suites generated successfully.")
