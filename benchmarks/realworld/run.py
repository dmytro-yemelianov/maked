#!/usr/bin/env python3
"""
Real-world compatibility suite: build real projects with maked and with
GNU make and compare what happens.

For each project, and for each tool in a fresh extract:
  1. configure (if the project has a configure step) and build with -j8;
  2. run a smoke test of the built program;
  3. null build: run the tool again; no file may change;
  4. incremental build: touch one header, rebuild, and record which files
     changed.
Then compare the tools: build and smoke results, the set of files each
build produced, and the set of files the incremental rebuild touched.

Usage: benchmarks/realworld/run.py [project ...]   (default: all)
Tarballs are cached in benchmarks/realworld/.cache, pinned by SHA-256.
"""
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CACHE = ROOT / "benchmarks/realworld/.cache"
MAKED = ROOT / "rust_make/target/release/maked"
GMAKE = shutil.which("gmake") or shutil.which("make")
JOBS = "-j8"
IS_MAC = platform.system() == "Darwin"

PROJECTS = {
    "zlib": {
        "url": "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
        "sha256": "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
        "configure": ["./configure"],
        "args": [],
        "smoke": ["./example"],
        "expect": "inflate with dictionary: hello, hello!",
        "touch": "zutil.h",
    },
    "sqlite": {
        "url": "https://www.sqlite.org/2024/sqlite-autoconf-3460100.tar.gz",
        "sha256": "67d3fe6d268e6eaddcae3727fce58fcc8e9c53869bdd07a0c61e38ddf2965071",
        "configure": ["./configure"],
        "args": [],
        "smoke": ["sh", "-c", "echo 'select 6*7;' | ./sqlite3"],
        "expect": "42",
        "touch": "sqlite3.h",
    },
    "redis": {
        "url": "https://download.redis.io/releases/redis-7.2.5.tar.gz",
        "sha256": "5981179706f8391f03be91d951acafaeda91af7fac56beffb2701963103e423d",
        "configure": None,
        "args": [],
        "smoke": ["src/redis-server", "--version"],
        "expect": "v=7.2.5",
        "touch": "src/server.h",
    },
    "git": {
        "url": "https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.46.0.tar.xz",
        "sha256": "7f123462a28b7ca3ebe2607485f7168554c2b10dfc155c7ec46300666ac27f95",
        "configure": None,
        "args": ["NO_GETTEXT=1", "NO_TCLTK=1", "NO_CURL=1", "NO_EXPAT=1", "NO_PERL=1", "NO_PYTHON=1"],
        "smoke": ["./git", "--version"],
        "expect": "git version 2.46.0",
        "touch": "git-compat-util.h",
    },
    "jq": {
        "url": "https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-1.7.1.tar.gz",
        "sha256": "478c9ca129fd2e3443fe27314b455e211e0d8c60bc8ff7df703873deeee580c2",
        "configure": ["./configure", "--with-oniguruma=builtin", "--disable-docs"],
        "args": [],
        "smoke": ["sh", "-c", "echo '{\"a\":[1,2]}' | ./jq '.a|add'"],
        "expect": "3",
        "touch": "src/jv.h",
    },
    "lua": {
        "local": ROOT / "benchmarks/lua_test/lua-5.4.9",
        "configure": None,
        "args": ["macosx" if IS_MAC else "linux"],
        "smoke": ["src/lua", "-e", "print(6*7)"],
        "expect": "42",
        "touch": "src/lua.h",
    },
}


def fetch(name, spec):
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / spec["url"].rsplit("/", 1)[1]
    if not path.exists():
        print(f"    fetching {spec['url']}")
        req = urllib.request.Request(spec["url"], headers={"User-Agent": "curl/8 (maked realworld suite)"})
        with urllib.request.urlopen(req, timeout=300) as r:
            path.write_bytes(r.read())
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != spec["sha256"]:
        raise SystemExit(f"{name}: sha256 mismatch: {digest}")
    return path


def extract(name, spec, dest):
    if "local" in spec:
        shutil.copytree(spec["local"], dest / name, symlinks=True)
        # A clean tree: drop objects a previous local build left behind.
        subprocess.run([GMAKE, "-s", "-C", str(dest / name / "src"), "clean"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return dest / name
    with tarfile.open(fetch(name, spec)) as t:
        top = t.getnames()[0].split("/")[0]
        t.extractall(dest)
    return dest / top


def snapshot(root):
    out = {}
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != ".git"]
        for f in filenames:
            if f == ".maked_log":  # maked's own scheduling history
                continue
            p = Path(dirpath) / f
            try:
                st = p.lstat()
            except FileNotFoundError:
                continue
            out[str(p.relative_to(root))] = st.st_mtime_ns
    return out


def changed(before, after):
    return sorted(k for k, v in after.items() if before.get(k) != v)


def run(cmd, cwd, log):
    t0 = time.monotonic()
    try:
        r = subprocess.run(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    except OSError as e:
        log.write(f"$ {' '.join(map(str, cmd))}\n{e}\n")
        return 127, time.monotonic() - t0, str(e)
    log.write(f"$ {' '.join(map(str, cmd))}\n{r.stdout}\n")
    return r.returncode, time.monotonic() - t0, r.stdout


def build_with(tool, name, spec, work, logdir):
    tree = extract(name, spec, work)
    log = open(logdir / f"{name}-{Path(tool).name}.log", "w")
    res = {"tool": Path(tool).name}
    env_tool = [str(tool)]
    if spec["configure"]:
        rc, _, _ = run(spec["configure"], tree, log)
        if rc != 0:
            res["error"] = "configure failed"
            return res
    pristine = snapshot(tree)
    rc, secs, _ = run(env_tool + [JOBS] + spec["args"], tree, log)
    res["build_ok"] = rc == 0
    res["build_seconds"] = round(secs, 2)
    built = snapshot(tree)
    res["produced"] = sorted(set(built) - set(pristine))
    rc, _, out = run(spec["smoke"], tree, log)
    res["smoke_ok"] = rc == 0 and spec["expect"] in out
    if not res["build_ok"]:
        return res

    time.sleep(1.1)  # make every later write visibly newer than the build
    before = snapshot(tree)
    rc, secs, _ = run(env_tool + [JOBS] + spec["args"], tree, log)
    res["null_ok"] = rc == 0
    res["null_seconds"] = round(secs, 2)
    res["null_changed"] = changed(before, snapshot(tree))

    time.sleep(1.1)
    (tree / spec["touch"]).touch()
    before = snapshot(tree)
    rc, secs, _ = run(env_tool + [JOBS] + spec["args"], tree, log)
    res["incr_ok"] = rc == 0
    res["incr_seconds"] = round(secs, 2)
    res["incr_changed"] = changed(before, snapshot(tree))
    rc, _, out = run(spec["smoke"], tree, log)
    res["smoke_after_incr_ok"] = rc == 0 and spec["expect"] in out
    return res


def main():
    names = sys.argv[1:] or list(PROJECTS)
    logdir = ROOT / "benchmarks/realworld/logs"
    logdir.mkdir(parents=True, exist_ok=True)
    report = {"host": f"{platform.system()} {platform.machine()}",
              "gnu_make": subprocess.run([GMAKE, "--version"], capture_output=True, text=True).stdout.splitlines()[0],
              "maked": subprocess.run([str(MAKED), "--version"], capture_output=True, text=True).stdout.splitlines()[0],
              "projects": {}}
    failures = []
    for name in names:
        spec = PROJECTS[name]
        print(f"==> {name}")
        results = {}
        for tool in (GMAKE, MAKED):
            with tempfile.TemporaryDirectory(prefix=f"rw_{name}_") as tmp:
                results[Path(tool).name] = build_with(tool, name, spec, Path(tmp), logdir)
        g, m = results[Path(GMAKE).name], results["maked"]
        problems = []
        for key in ("build_ok", "smoke_ok", "null_ok", "incr_ok", "smoke_after_incr_ok"):
            if m.get(key) is not True:
                problems.append(f"maked {key}={m.get(key)} (GNU make: {g.get(key)})")
        if set(m.get("null_changed", [])) != set(g.get("null_changed", [])):
            only_m = sorted(set(m.get("null_changed", [])) - set(g.get("null_changed", [])))
            problems.append(f"null build differs: maked also changed {len(only_m)} files: {only_m[:5]}")
        if set(m.get("produced", [])) != set(g.get("produced", [])):
            only_m = sorted(set(m["produced"]) - set(g["produced"]))
            only_g = sorted(set(g["produced"]) - set(m["produced"]))
            problems.append(f"produced files differ: only maked {only_m[:5]}, only GNU {only_g[:5]}")
        if set(m.get("incr_changed", [])) != set(g.get("incr_changed", [])):
            only_m = sorted(set(m["incr_changed"]) - set(g["incr_changed"]))
            only_g = sorted(set(g["incr_changed"]) - set(m["incr_changed"]))
            problems.append(f"incremental rebuild differs: only maked {only_m[:5]}, only GNU {only_g[:5]}")
        for tool, r in results.items():
            print(f"    {tool:7} build {r.get('build_seconds', '-')}s ok={r.get('build_ok')} "
                  f"smoke={r.get('smoke_ok')} null {r.get('null_seconds', '-')}s "
                  f"({len(r.get('null_changed', []))} changed) "
                  f"incr {r.get('incr_seconds', '-')}s ({len(r.get('incr_changed', []))} changed)")
        for p in problems:
            print(f"    [-] {p}")
        if not problems:
            print("    [+] same as GNU make")
        report["projects"][name] = {"results": {k: {kk: vv for kk, vv in v.items() if kk != "produced"}
                                                for k, v in results.items()},
                                    "problems": problems}
        failures += [f"{name}: {p}" for p in problems]
    out = ROOT / "benchmarks/realworld/report.json"
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\n{len(names) - len({f.split(':')[0] for f in failures})}/{len(names)} projects match GNU make; report: {out}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
