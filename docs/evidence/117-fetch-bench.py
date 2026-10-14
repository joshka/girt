#!/usr/bin/env python3
"""Times a one-commit fetch with jj on girt, stock jj and Git.

Usage: 117-fetch-bench.py --girt NAME=JJ [--girt NAME=JJ ...] --stock JJ --repo NAME=BARE
       [--runs N] [--work DIR] [--out FILE.jsonl]

For each repo and tool it makes a base: a copy of the bare origin and a clone made by that tool
(`jj git clone` or `git clone --no-local`), and records the clone's loose/packed reference counts.
Each sample copies the base with `cp -cR` (APFS clone), adds one origin commit that adds one blob,
and times the fetch. `fresh` samples time the copy as made; `warm` samples first read every
reference, `packed-refs`, pack index and `.jj` file once. Every sample holds the shared timing lock
from copy to timing, so no other timing run overlaps its I/O. Failures stop the run. Each sample is
appended as one JSON line with the binaries' SHA-256.

jj runs with JJ_CONFIG=/dev/null and Git with an empty global and system config, so neither
starts user-configured daemons or maintenance.

Requires macOS (`/usr/bin/time -l`, `shlock`, `cp -c`). Original script; no upstream code used.
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys, time
from pathlib import Path

LOCK = "/tmp/girt-timing.lock"
GIT_ENV = {"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
JJ_ENV = {"JJ_CONFIG": "/dev/null"}


def run(cmd, cwd, env=None, check=True):
    p = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})},
                       capture_output=True, text=True)
    if check and p.returncode != 0:
        sys.exit(f"failed in {cwd}: {cmd}\n{p.stderr}")
    return p.stdout.strip()


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def clone(tool, binary, base):
    if tool == "git":
        run(["git", "clone", "-q", "--no-local", "origin.git", "repo"], base, GIT_ENV)
    else:
        run([binary, "--config", "user.name=b", "--config", "user.email=b@b", "git", "clone",
             "origin.git", "repo"], base, JJ_ENV)


def layout(repo):
    git_dir = Path(repo) / ".git"
    loose = sum(1 for p in (git_dir / "refs").rglob("*") if p.is_file())
    packed = git_dir / "packed-refs"
    lines = packed.read_text().count("\n") if packed.exists() else 0
    return {"loose_refs": loose, "packed_refs_lines": lines}


def add_commit(origin):
    branch = run(["git", "symbolic-ref", "--short", "HEAD"], origin, GIT_ENV, check=False) \
        or "master"
    index = {"GIT_INDEX_FILE": str(Path(origin) / "bench-index"), **GIT_ENV}
    run(["git", "read-tree", branch], origin, index)
    blob = subprocess.run(["git", "hash-object", "-w", "--stdin"], cwd=origin,
                          input=f"bench-{time.time_ns()}\n", capture_output=True, text=True,
                          check=True, env={**os.environ, **GIT_ENV}).stdout.strip()
    run(["git", "update-index", "--add", "--cacheinfo", f"100644,{blob},bench-fetch.txt"],
        origin, index)
    tree = run(["git", "write-tree"], origin, index)
    commit = run(["git", "commit-tree", tree, "-p", branch, "-m", "bench fetch"], origin, GIT_ENV)
    run(["git", "update-ref", f"refs/heads/{branch}", commit], origin, GIT_ENV)
    os.remove(index["GIT_INDEX_FILE"])


def warm(repo):
    for top in (Path(repo) / ".git", Path(repo) / ".jj"):
        for p in top.rglob("*"):
            if p.is_file() and ("refs" in p.parts or p.name == "packed-refs"
                                or p.suffix == ".idx" or ".jj" in p.parts):
                p.read_bytes()


def timed(cmd, cwd, env):
    p = subprocess.run(["/usr/bin/time", "-l"] + cmd, cwd=cwd,
                       env={**os.environ, **env}, capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit(f"timed command failed in {cwd}: {cmd}\n{p.stderr}")
    real = float(re.search(r"([\d.]+) real", p.stderr).group(1))
    rss = int(re.search(r"(\d+)\s+maximum resident set size", p.stderr).group(1)) / 2**20
    return real, rss


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--girt", action="append", required=True)
    ap.add_argument("--stock", required=True)
    ap.add_argument("--repo", action="append", required=True)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--work", default="/tmp/girt-117-bench")
    ap.add_argument("--out", default="117-fetch.jsonl")
    a = ap.parse_args()
    tools = {name: path for name, path in (g.split("=", 1) for g in a.girt)}
    tools["stock"] = a.stock
    tools["git"] = None
    prints = {t: sha256(p) for t, p in tools.items() if p}
    prints["git"] = run(["git", "--version"], ".")
    work = Path(a.work)
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    out = open(a.out, "a")
    for spec in a.repo:
        repo_name, origin = spec.split("=", 1)
        bases = {}
        for tool, binary in tools.items():
            base = work / f"base-{repo_name}-{tool}"
            base.mkdir()
            run(["cp", "-cR", origin, str(base / "origin.git")], ".")
            clone("git" if tool == "git" else "jj", binary, base)
            bases[tool] = (base, layout(base / "repo"))
        for condition in ("fresh", "warm"):
            order = list(tools)
            for i in range(a.runs):
                for tool in order[i % len(order):] + order[:i % len(order)]:
                    base, refs = bases[tool]
                    sample = work / "sample"
                    while subprocess.run(["shlock", "-f", LOCK, "-p", str(os.getpid())]).returncode:
                        time.sleep(15)
                    try:
                        shutil.rmtree(sample, ignore_errors=True)
                        run(["cp", "-cR", str(base), str(sample)], ".")
                        add_commit(sample / "origin.git")
                        if condition == "warm":
                            warm(sample / "repo")
                        if tool == "git":
                            real, rss = timed(["git", "fetch", "-q"], sample / "repo", GIT_ENV)
                        else:
                            real, rss = timed([tools[tool], "git", "fetch"], sample / "repo", JJ_ENV)
                    finally:
                        os.remove(LOCK)
                    record = dict(repo=repo_name, tool=tool, condition=condition, run=i,
                                  seconds=real, rss_mib=round(rss, 1), binary=prints[tool], **refs)
                    out.write(json.dumps(record) + "\n")
                    out.flush()
                    print(record, flush=True)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
