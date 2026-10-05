#!/usr/bin/env python3
"""
Massive DAG Generator for Build System Scalability Benchmarks.
Generates both Makefile and build.ninja for identical dependency graphs
with configurable node counts (1,000 to 50,000+ nodes) and topologies:
  1. Wide Fan-Out: 1 root depending on N leaf targets.
  2. Deep Pipeline: Linear chain of length N.
  3. Diamond Mesh / Lattice: Multi-layer criss-cross graph.
  4. Hierarchical Modular: Realistic compiler project (packages, libraries, binaries).
"""

import os
import sys
import argparse

def generate_wide_dag(out_dir, n):
    os.makedirs(out_dir, exist_ok=True)
    mf_path = os.path.join(out_dir, "Makefile")
    ninja_path = os.path.join(out_dir, "build.ninja")

    # Generate source files
    src_dir = os.path.join(out_dir, "src")
    os.makedirs(src_dir, exist_ok=True)

    targets = [f"obj_{i}.o" for i in range(n)]

    with open(mf_path, "w") as f_mf, open(ninja_path, "w") as f_nj:
        # Makefile
        f_mf.write("# Wide Fan-out Makefile\n")
        f_mf.write(".PHONY: all\n")
        f_mf.write(f"all: {' '.join(targets)}\n\n")

        # Ninja
        f_nj.write("# Wide Fan-out Ninja\n")
        f_nj.write("rule touch_rule\n")
        f_nj.write("  command = touch $out\n\n")

        for i in range(n):
            src = f"src/src_{i}.c"
            with open(os.path.join(out_dir, src), "w") as sf:
                sf.write(f"int val_{i} = {i};\n")
            obj = f"obj_{i}.o"
            f_mf.write(f"{obj}: {src}\n\t@touch $@\n\n")
            f_nj.write(f"build {obj}: touch_rule {src}\n")

        f_nj.write(f"\nbuild all: phony {' '.join(targets)}\n")
        f_nj.write("default all\n")

def generate_deep_dag(out_dir, n):
    os.makedirs(out_dir, exist_ok=True)
    mf_path = os.path.join(out_dir, "Makefile")
    ninja_path = os.path.join(out_dir, "build.ninja")

    with open(mf_path, "w") as f_mf, open(ninja_path, "w") as f_nj:
        f_mf.write("# Deep Linear Pipeline Makefile\n")
        f_mf.write(f".PHONY: all\nall: node_{n-1}\n\n")

        f_nj.write("# Deep Linear Pipeline Ninja\n")
        f_nj.write("rule step_rule\n")
        f_nj.write("  command = touch $out\n\n")

        # Leaf
        leaf = "node_0"
        with open(os.path.join(out_dir, "leaf.txt"), "w") as sf:
            sf.write("seed\n")
        f_mf.write(f"node_0: leaf.txt\n\t@touch $@\n\n")
        f_nj.write(f"build node_0: step_rule leaf.txt\n")

        for i in range(1, n):
            prev = f"node_{i-1}"
            curr = f"node_{i}"
            f_mf.write(f"{curr}: {prev}\n\t@touch $@\n\n")
            f_nj.write(f"build {curr}: step_rule {prev}\n")

        f_nj.write(f"\nbuild all: phony node_{n-1}\n")
        f_nj.write("default all\n")

def generate_diamond_dag(out_dir, layers, width):
    os.makedirs(out_dir, exist_ok=True)
    mf_path = os.path.join(out_dir, "Makefile")
    ninja_path = os.path.join(out_dir, "build.ninja")

    total_nodes = layers * width

    with open(mf_path, "w") as f_mf, open(ninja_path, "w") as f_nj:
        f_mf.write(f"# Diamond Lattice Makefile: {layers} layers x {width} width ({total_nodes} nodes)\n")
        f_mf.write(f".PHONY: all\n")

        top_targets = [f"node_{layers-1}_{w}" for w in range(width)]
        f_mf.write(f"all: {' '.join(top_targets)}\n\n")

        f_nj.write(f"# Diamond Lattice Ninja\n")
        f_nj.write("rule touch_rule\n")
        f_nj.write("  command = touch $out\n\n")

        # Layer 0 leaves
        with open(os.path.join(out_dir, "seed.txt"), "w") as sf:
            sf.write("seed\n")

        for w in range(width):
            f_mf.write(f"node_0_{w}: seed.txt\n\t@touch $@\n\n")
            f_nj.write(f"build node_0_{w}: touch_rule seed.txt\n")

        for l in range(1, layers):
            for w in range(width):
                curr = f"node_{l}_{w}"
                # Depends on w and (w+1)%width from previous layer
                p1 = f"node_{l-1}_{w}"
                p2 = f"node_{l-1}_{(w+1)%width}"
                f_mf.write(f"{curr}: {p1} {p2}\n\t@touch $@\n\n")
                f_nj.write(f"build {curr}: touch_rule {p1} {p2}\n")

        f_nj.write(f"\nbuild all: phony {' '.join(top_targets)}\n")
        f_nj.write("default all\n")

def generate_modular_dag(out_dir, num_modules, files_per_mod):
    """
    Realistic modular architecture:
    num_modules packages, each having files_per_mod C objects compiled,
    bundled into libmod_X.a, and finally linked into app binary.
    """
    os.makedirs(out_dir, exist_ok=True)
    mf_path = os.path.join(out_dir, "Makefile")
    ninja_path = os.path.join(out_dir, "build.ninja")

    all_libs = []
    all_objs = []

    with open(mf_path, "w") as f_mf, open(ninja_path, "w") as f_nj:
        f_mf.write("# Modular Realistic Compiler Makefile\n")
        f_mf.write(".PHONY: all clean\n\n")

        f_nj.write("# Modular Realistic Compiler Ninja\n")
        f_nj.write("rule touch_rule\n")
        f_nj.write("  command = touch $out\n\n")

        for m in range(num_modules):
            mod_dir = os.path.join(out_dir, f"mod_{m}")
            os.makedirs(mod_dir, exist_ok=True)
            mod_objs = []
            for i in range(files_per_mod):
                src = f"mod_{m}/file_{i}.c"
                obj = f"mod_{m}/file_{i}.o"
                with open(os.path.join(out_dir, src), "w") as sf:
                    sf.write(f"int mod_{m}_fn_{i}() {{ return {m * 1000 + i}; }}\n")
                mod_objs.append(obj)
                all_objs.append(obj)
                f_mf.write(f"{obj}: {src}\n\t@touch $@\n\n")
                f_nj.write(f"build {obj}: touch_rule {src}\n")

            lib = f"mod_{m}/libmod_{m}.a"
            all_libs.append(lib)
            f_mf.write(f"{lib}: {' '.join(mod_objs)}\n\t@touch $@\n\n")
            f_nj.write(f"build {lib}: touch_rule {' '.join(mod_objs)}\n")

        # Root app binary
        f_mf.write(f"app: {' '.join(all_libs)}\n\t@touch $@\n\n")
        f_mf.write("all: app\n\n")
        f_mf.write(f"clean:\n\t@rm -f app {' '.join(all_libs)} {' '.join(all_objs)}\n")

        f_nj.write(f"build app: touch_rule {' '.join(all_libs)}\n")
        f_nj.write("build all: phony app\n")
        f_nj.write("default all\n")

def main():
    parser = argparse.ArgumentParser(description="Generate massive DAG benchmarks")
    parser.add_argument("--topology", choices=["wide", "deep", "diamond", "modular"], default="modular")
    parser.add_argument("--size", type=int, default=5000, help="Target count")
    parser.add_argument("--out", type=str, default="/tmp/massive_dag_bench", help="Output directory")
    args = parser.parse_args()

    print(f"[*] Generating {args.topology} DAG with ~{args.size} nodes into {args.out}...")
    if args.topology == "wide":
        generate_wide_dag(args.out, args.size)
    elif args.topology == "deep":
        generate_deep_dag(args.out, args.size)
    elif args.topology == "diamond":
        # e.g., width 50, layers = size // 50
        width = 50
        layers = max(2, args.size // width)
        generate_diamond_dag(args.out, layers, width)
    elif args.topology == "modular":
        files_per_mod = 25
        num_mods = max(1, args.size // files_per_mod)
        generate_modular_dag(args.out, num_mods, files_per_mod)

    print(f"[+] Successfully generated Makefile and build.ninja in {args.out}")

if __name__ == "__main__":
    main()
