#!/usr/bin/env python3
"""Times clone of bare repos with jj builds and Git; 5 runs per tool, order rotated, lock per run."""
import json, os, re, shutil, statistics, subprocess, sys, time

LOCK = "/tmp/girt-timing.lock"
work = "/tmp/b119/bench"
repos = {"git-git": "/tmp/b119/origin.git", "jj-fixture": "/tmp/opus-med-23/source.git"}
tools = {
    "girt-new": "/tmp/b119/jj-final",
    "girt-main": "/tmp/b119/jj-main",
    "stock": "/tmp/b119/jj-stock",
    "git": None,
}
runs = int(sys.argv[1]) if len(sys.argv) > 1 else 5

def cmd(tool, origin, dest):
    if tool == "git":
        return ["git", "clone", "-q", "--no-local", origin, dest]
    return [tools[tool], "--config", "user.name=b", "--config", "user.email=b@b", "git", "clone", origin, dest]

def locked(argv):
    while subprocess.run(["shlock", "-f", LOCK, "-p", str(os.getpid())]).returncode != 0:
        time.sleep(15)
    try:
        p = subprocess.run(["/usr/bin/time", "-l"] + argv, capture_output=True, text=True,
                           env={**os.environ, "JJ_CONFIG": "/dev/null"})
    finally:
        os.remove(LOCK)
    real = float(re.search(r"([\d.]+) real", p.stderr).group(1))
    rss = int(re.search(r"(\d+)\s+maximum resident set size", p.stderr).group(1)) / 2**20
    return p.returncode, real, rss, p.stderr

results = []
os.makedirs(work, exist_ok=True)
for repo, origin in repos.items():
    order = list(tools)
    for i in range(runs):
        for tool in order[i % len(order):] + order[: i % len(order)]:
            dest = f"{work}/{repo}-{tool}"
            shutil.rmtree(dest, ignore_errors=True)
            code, real, rss, err = locked(cmd(tool, origin, dest))
            if code != 0:
                sys.exit(f"{tool} {repo} failed: {err}")
            results.append(dict(repo=repo, tool=tool, run=i, seconds=real, rss_mib=rss))
            print(f"{repo} {tool} run {i}: {real:.2f}s {rss:.0f} MiB", flush=True)
    for tool in ("girt-new", "girt-main", "stock"):
        dest = f"{work}/{repo}-{tool}"
        fsck = subprocess.run(["git", "-C", dest, "fsck", "--strict", "--no-progress"], capture_output=True)
        print(f"{repo} {tool} fsck: {'ok' if fsck.returncode == 0 else fsck.stderr.decode()[:200]}")
json.dump(results, open("/tmp/b119/bench-clone.json", "w"), indent=1)
print("| repo | tool | median s | range s | peak RSS MiB |")
print("|---|---|---:|---:|---:|")
for repo in repos:
    for tool in tools:
        rows = [r for r in results if r["repo"] == repo and r["tool"] == tool]
        s = [r["seconds"] for r in rows]
        print(f"| {repo} | {tool} | {statistics.median(s):.2f} | {min(s):.2f}–{max(s):.2f} | {max(r['rss_mib'] for r in rows):.0f} |")
