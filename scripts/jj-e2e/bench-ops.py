#!/usr/bin/env python3
"""Compares everyday jj operations on girt against stock jj and the closest Git command.

Each run copies a bare origin repository afresh and executes one operation sequence with each of
three tools: jj built on girt, stock jj (built from the same jj commit, on gix), and Git. Every
operation executes in every run so later operations see the same state; `--ops` only selects which
ones are recorded. The tool order rotates between runs, so use a multiple of three runs to balance
positions. Each run holds the shared timing lock (/tmp/girt-timing.lock) while it executes. Setup
steps are untimed. After the lock is released, the girt clone and its push destination are checked
with a full `git fsck`.

Caches are not controlled: the history scan that selects inputs, earlier runs and earlier
operations in a run all warm the file cache, so results are warm-cache figures.

Some rows compare different work; their `workload` field says how (see WORKLOAD_NOTES).

Exits nonzero if any measured command fails or any fsck fails. A failed command invalidates the
rest of that run; its directory is kept and its path printed. Other run directories are removed.

Requirements: macOS (`/usr/bin/time -l`, `shlock`, `cp -c` on APFS), Git, Python 3.9+.

Usage:
  bench-ops.py --girt-jj PATH --stock-jj PATH --repo NAME=BARE_REPO [--repo ...]
               [--runs 6] [--work DIR] [--out summary.json] [--ops op,op] [--girt-rev REV]

Each NAME=BARE_REPO is a bare Git repository whose HEAD names a branch. Copies use `cp -c` (APFS
clones), so the origin is never mutated. See docs/jj-replacement-status.md for building the two jj
binaries.
"""
import argparse
import atexit
import collections
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

LOCK = "/tmp/girt-timing.lock"
TOOLS = ("girt", "stock", "git")
STACK = 10
WORKLOAD_NOTES = {
    "clone": "git uses --no-local, so it transfers a pack like jj does",
    "log-default": "jj default revset with graph vs git log --graph -n 20",
    "annotate": "git blame",
    "gc": "jj util gc on girt skips verified no-ops; stock jj and git gc repack",
}


def env():
    e = dict(os.environ)
    e.update(
        JJ_CONFIG="/dev/null",
        JJ_USER="bench",
        JJ_EMAIL="bench@example.com",
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="bench",
        GIT_AUTHOR_EMAIL="bench@example.com",
        GIT_COMMITTER_NAME="bench",
        GIT_COMMITTER_EMAIL="bench@example.com",
        GIT_PAGER="cat",
        PAGER="cat",
    )
    return e


ENV = env()
q = shlex.quote


def sh(cmd, cwd, extra_env=None):
    """Runs untimed setup; raises on failure."""
    p = subprocess.run(["sh", "-c", cmd], cwd=cwd, env={**ENV, **(extra_env or {})},
                       capture_output=True)
    if p.returncode:
        raise RuntimeError(f"setup failed in {cwd}: {cmd}\n{p.stderr.decode()[-800:]}")
    return p.stdout.decode().strip()


def git_z(args, cwd):
    """Runs git and splits NUL-delimited output, preserving paths with spaces."""
    p = subprocess.run(["git", *args], cwd=cwd, env=ENV, capture_output=True, check=True)
    return [x.decode() for x in p.stdout.split(b"\0") if x.strip(b"\n")]


def timed(cmd, cwd):
    """Runs `cmd` under /usr/bin/time -l; returns wall ms, peak RSS MiB and status."""
    start = time.perf_counter()
    p = subprocess.run(["/usr/bin/time", "-l", "sh", "-c", cmd + " >/dev/null"], cwd=cwd,
                       env=ENV, capture_output=True, text=True)
    ms = (time.perf_counter() - start) * 1000
    m = re.search(r"(\d+)\s+maximum resident set size", p.stderr)
    rss = int(m.group(1)) / 1048576 if m else 0.0
    return dict(ms=ms, rss_mb=rss, status=p.returncode, err=p.stderr[-600:])


class Lock:
    """The machine-wide timing lock, held only while a run executes."""

    held = False

    def acquire(self):
        while subprocess.run(["shlock", "-f", LOCK, "-p", str(os.getpid())]).returncode:
            time.sleep(15)
        self.held = True

    def release(self):
        if self.held:
            try:
                os.remove(LOCK)
            except FileNotFoundError:
                pass
            self.held = False


LOCK_STATE = Lock()
atexit.register(LOCK_STATE.release)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))


def preflight(args):
    for tool in ("shlock", "git", "cp"):
        if not shutil.which(tool):
            sys.exit(f"missing required tool: {tool}")
    if not os.access("/usr/bin/time", os.X_OK):
        sys.exit("missing /usr/bin/time (macOS required)")
    for path in (args.girt_jj, args.stock_jj):
        if not os.access(path, os.X_OK):
            sys.exit(f"not an executable jj: {path}")
    for spec in args.repo:
        if "=" not in spec:
            sys.exit(f"--repo needs NAME=BARE_REPO: {spec}")
        origin = spec.split("=", 1)[1]
        p = subprocess.run(["git", "-C", origin, "symbolic-ref", "-q", "HEAD"], env=ENV,
                           capture_output=True)
        if p.returncode:
            sys.exit(f"{origin}: HEAD must name a branch (detached or not a repository)")
    probe = Path(tempfile.mkdtemp(dir=args.work))
    try:
        (probe / "a").write_text("")
        if subprocess.run(["cp", "-c", probe / "a", probe / "b"],
                          capture_output=True).returncode:
            sys.exit(f"{args.work}: `cp -c` clones unsupported (needs APFS)")
    finally:
        shutil.rmtree(probe)


def provenance(args, jj):
    def out(cmd):
        return subprocess.run(cmd, env=ENV, capture_output=True, text=True).stdout.strip()

    def sha256(path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()

    return dict(
        argv=sys.argv,
        started=time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        platform=platform.platform(),
        machine=out(["sysctl", "-n", "machdep.cpu.brand_string"]),
        load_average=os.getloadavg(),
        python=platform.python_version(),
        git=out(["git", "--version"]),
        girt_jj=dict(path=jj["girt"], version=out([jj["girt"], "--version"]),
                     sha256=sha256(jj["girt"]), girt_rev=args.girt_rev),
        stock_jj=dict(path=jj["stock"], version=out([jj["stock"], "--version"]),
                      sha256=sha256(jj["stock"])),
        cache="uncontrolled, warm: input scan, earlier runs and earlier operations warm it",
    )


def facts(origin):
    """Per-repository inputs chosen from history: large diff, annotate file, rebase target."""
    branch = sh("git symbolic-ref --short HEAD", origin)
    big = None
    out = sh(f"git log -n 3000 --no-merges --format=%H --shortstat {q(branch)}", origin)
    for commit, stat in re.findall(r"([0-9a-f]{40})\n\n\s*(\d+) files? changed", out):
        if big is None or int(stat) > big[1]:
            big = (commit, int(stat))
    names = git_z(["log", "-z", "-n", "1000", "--no-merges", "--format=", "--name-only", branch],
                  origin)
    tracked = set(git_z(["ls-tree", "-z", "-r", "--name-only", branch], origin))
    counts = collections.Counter(n.strip("\n") for n in names if n.strip("\n") in tracked)
    return dict(
        branch=branch,
        head=sh(f"git rev-parse {q(branch)}", origin),
        diff_commit=big[0],
        diff_files=big[1],
        annotate=counts.most_common(1)[0][0],
        rebase_dest=sh(f"git rev-parse {q(branch + '~20')}", origin),
        objects=sh("git count-objects -v | awk '/in-pack/{print $2}'", origin),
        refs=sh("git for-each-ref | wc -l", origin).strip(),
    )


def add_origin_commit(origin, branch):
    """Advances the origin's branch by one commit that adds one new blob."""
    index = {"GIT_INDEX_FILE": str(Path(origin) / "bench-index")}
    sh(f"git read-tree {q(branch)}", origin, index)
    blob = sh(f"echo bench-{time.time_ns()} | git hash-object -w --stdin", origin)
    sh(f"git update-index --add --cacheinfo 100644,{blob},bench-fetch.txt", origin, index)
    tree = sh("git write-tree", origin, index)
    commit = sh(f"git commit-tree {tree} -p {q(branch)} -m 'bench fetch'", origin)
    sh(f"git update-ref refs/heads/{q(branch)} {commit}", origin)
    os.remove(index["GIT_INDEX_FILE"])


def operations(tool, jj, f):
    """(name, setup, timed command, cwd) in order. Setup runs untimed before the command.

    The states match across tools: `status-dirty` leaves an edited and a new file; `new` starts
    from those changes committed (jj's snapshot already did that, git commits them in setup) and
    creates an empty child, which `describe` then rewrites. The rebase moves exactly ten commits.
    """
    b, ann = f["branch"], q(f["annotate"])
    dirty = f"echo dirty >> {ann} && echo new > dirty-new.txt"
    repo = "repo"
    if tool == "git":
        stack = " && ".join(
            f"echo {i} > stack-{i}.txt && git add stack-{i}.txt && git commit -qm 'stack {i}'"
            for i in range(STACK))
        return [
            ("clone", None, "git clone -q --no-local origin.git repo", "."),
            ("fetch-noop", None, "git fetch -q", repo),
            ("fetch-1", "ORIGIN_COMMIT", "git fetch -q", repo),
            ("status-clean", None, "git status", repo),
            ("status-dirty", dirty, "git status", repo),
            ("log-default", None, "git log --graph --oneline --decorate -n 20", repo),
            ("log-ancestry-1000", None, "git log -n 1000 --format=%H", repo),
            ("diff-large", None, f"git diff {f['diff_commit']}^ {f['diff_commit']}", repo),
            ("new", "git add -A && git commit -qm wip", "git commit -q --allow-empty -m new",
             repo),
            ("describe", None, "git commit -q --amend --allow-empty -m described", repo),
            ("rebase-stack-10", f"git checkout -q -b stack origin/{q(b)} && {stack}",
             f"git rebase -q --onto {f['rebase_dest']} origin/{q(b)} stack", repo),
            ("push-new-bookmark", None, "git push -q origin stack:refs/heads/bench", repo),
            ("annotate", None, f"git blame -- {ann}", repo),
            ("gc", None, "git gc -q", repo),
        ]
    jj = q(jj)
    # `jj new -m` then a file write: the next command snapshots the write, so the stack is
    # exactly ten commits with the working-copy commit as its top.
    parents = [q(b + "@origin")] + [""] * (STACK - 1)
    stack = " && ".join(f"{jj} new {parent} -m 'stack {i}' && echo {i} > stack-{i}.txt"
                        for i, parent in enumerate(parents))
    return [
        ("clone", None, f"{jj} git clone origin.git repo", "."),
        ("fetch-noop", None, f"{jj} git fetch", repo),
        ("fetch-1", "ORIGIN_COMMIT", f"{jj} git fetch", repo),
        ("status-clean", None, f"{jj} status", repo),
        ("status-dirty", dirty, f"{jj} status", repo),
        ("log-default", None, f"{jj} log", repo),
        ("log-ancestry-1000", None,
         f"{jj} log -r '::@' --limit 1000 --no-graph -T 'commit_id ++ \"\\n\"'", repo),
        ("diff-large", None, f"{jj} diff --git -r {f['diff_commit']}", repo),
        ("new", None, f"{jj} new", repo),
        ("describe", None, f"{jj} describe -m described", repo),
        ("rebase-stack-10", stack,
         f"{jj} rebase -s {q(f'roots({b}@origin..@)')} -d {f['rebase_dest']}", repo),
        ("push-new-bookmark", f"{jj} bookmark create bench -r @",
         f"{jj} git push -b bench", repo),
        ("annotate", None, f"{jj} file annotate {ann}", repo),
        ("gc", None, f"{jj} util gc", repo),
    ]


def git_dir(work, tool):
    repo = work / "repo"
    if tool == "git" or (repo / ".git").is_dir():
        return repo / ".git"
    return repo / ".jj" / "repo" / "store" / "git"


def run_once(name, origin, tool, jj, f, work, only):
    """Executes every operation; records the selected ones. Returns (records, ok)."""
    subprocess.run(["cp", "-cR", origin, work / "origin.git"], check=True)
    records = []
    LOCK_STATE.acquire()
    try:
        for op, setup, cmd, cwd in operations(tool, jj, f):
            cwd = work / cwd
            if setup == "ORIGIN_COMMIT":
                add_origin_commit(work / "origin.git", f["branch"])
            elif setup:
                sh(setup, cwd)
            rec = timed(cmd, cwd)
            if only and op not in only:
                if rec["status"]:
                    print(f"{name} {tool} {op} (unrecorded) failed:\n{rec['err']}", flush=True)
                    return records, False
                continue
            rec.update(repo=name, op=op, tool=tool)
            records.append(rec)
            print(f"{name:10} {tool:5} {op:18} {rec['ms']:9.1f}ms {rec['rss_mb']:7.1f}MiB"
                  f" status={rec['status']}", flush=True)
            if rec["status"]:
                print(rec["err"], flush=True)
                return records, False
    finally:
        LOCK_STATE.release()
    return records, True


def fsck(work, tool):
    """Full fsck of the clone and the push destination, outside the timing lock."""
    results = {}
    for label, path in (("clone", git_dir(work, tool)), ("origin", work / "origin.git")):
        p = subprocess.run(["git", "-C", path, "fsck", "--no-progress", "--no-dangling"],
                           env=ENV, capture_output=True, text=True)
        results[label] = p.returncode == 0
        if p.returncode:
            print(f"fsck FAILED {path}: {p.stderr[-400:]}", flush=True)
    return results


def summarize(records, fsck_runs):
    by = collections.defaultdict(list)
    for r in records:
        if r["status"] == 0:
            by[(r["repo"], r["op"], r["tool"])].append(r)
    keys = list(dict.fromkeys((r["repo"], r["op"]) for r in records))
    out = []
    for repo, op in keys:
        row = dict(repo=repo, op=op)
        notes = []
        for tool, key in (("girt", "girt_ms"), ("stock", "stock_jj_ms"), ("git", "git_ms")):
            rs = by.get((repo, op, tool), [])
            if not rs:
                row[key] = None
                notes.append(f"{tool}: failed")
                continue
            t = [x["ms"] for x in rs]
            row[key] = round(statistics.median(t), 1)
            notes.append(f"{tool} {min(t):.0f}-{max(t):.0f}ms n={len(t)}"
                         f" rss {max(x['rss_mb'] for x in rs):.0f}MiB")
        girt = by.get((repo, op, "girt"), [])
        row["girt_rss_mb"] = round(max(x["rss_mb"] for x in girt), 1) if girt else None
        runs = [r for r in fsck_runs if r["repo"] == repo]
        ok = all(all(r["result"].values()) for r in runs)
        state = "not run" if not runs else "ok" if ok else "FAILED"
        notes.append(f"girt fsck {state} ({len(runs)} runs)")
        row["workload"] = WORKLOAD_NOTES.get(op, "")
        row["notes"] = "; ".join(notes)
        out.append(row)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--girt-jj", required=True)
    ap.add_argument("--stock-jj", required=True)
    ap.add_argument("--repo", action="append", required=True, help="NAME=BARE_REPO")
    ap.add_argument("--runs", type=int, default=6, help="multiple of 3 balances tool order")
    ap.add_argument("--work", default="/tmp/jj-bench")
    ap.add_argument("--out", default="~/tmp/jj-bench.json")
    ap.add_argument("--ops", default="", help="comma-separated operations to record")
    ap.add_argument("--girt-rev", default=None, help="girt revision the girt jj was built with")
    args = ap.parse_args()
    out = Path(args.out).expanduser().resolve()
    raw_out = out.with_name(out.stem + "-raw.json")
    Path(args.work).mkdir(parents=True, exist_ok=True)
    preflight(args)
    jj = {"girt": os.path.abspath(args.girt_jj), "stock": os.path.abspath(args.stock_jj),
          "git": None}
    only = set(filter(None, args.ops.split(",")))
    session = Path(tempfile.mkdtemp(prefix="bench-ops-", dir=args.work))
    meta = provenance(args, jj)
    records, fsck_runs, failed_runs = [], [], []
    repos = [spec.split("=", 1) for spec in args.repo]
    repo_facts = {name: facts(origin) for name, origin in repos}
    for name in repo_facts:
        print(f"{name}: {json.dumps(repo_facts[name])}", flush=True)
    try:
        for name, origin in repos:
            for run in range(args.runs):
                order = TOOLS[run % 3:] + TOOLS[:run % 3]
                for tool in order:
                    work = session / f"{name}-{tool}-{run}"
                    work.mkdir()
                    keep = True
                    try:
                        recs, ok = run_once(name, origin, tool, jj[tool], repo_facts[name], work,
                                            only)
                        for rec in recs:
                            rec["run"] = run
                        records.extend(recs)
                        if not ok:
                            failed_runs.append(dict(repo=name, tool=tool, run=run,
                                                    kept=str(work)))
                            continue
                        if tool == "girt":
                            result = fsck(work, tool)
                            fsck_runs.append(dict(repo=name, run=run, result=result))
                            if not all(result.values()):
                                failed_runs.append(dict(repo=name, tool=tool, run=run,
                                                        kept=str(work), fsck=result))
                                continue
                        keep = False
                    finally:
                        if not keep:
                            shutil.rmtree(work, ignore_errors=True)
                        elif not any(f["kept"] == str(work) for f in failed_runs):
                            failed_runs.append(dict(repo=name, tool=tool, run=run,
                                                    kept=str(work), error="interrupted"))
    finally:
        summary = summarize(records, fsck_runs)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(summary, indent=1))
        raw_out.write_text(json.dumps(dict(provenance=meta, facts=repo_facts, records=records,
                                           fsck=fsck_runs, failed_runs=failed_runs), indent=1))
        if not failed_runs:
            shutil.rmtree(session, ignore_errors=True)
    print("\n| repo | op | jj+girt ms | stock jj ms | git ms | girt/stock | girt/git |"
          " girt RSS MiB | workload |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---|")
    ratio = lambda a, b: f"{a / b:.2f}x" if a and b else "-"
    for r in summary:
        print(f"| {r['repo']} | {r['op']} | {r['girt_ms']} | {r['stock_jj_ms']} | {r['git_ms']}"
              f" | {ratio(r['girt_ms'], r['stock_jj_ms'])} | {ratio(r['girt_ms'], r['git_ms'])}"
              f" | {r['girt_rss_mb']} | {r['workload']} |")
    fsck_ok = all(all(r["result"].values()) for r in fsck_runs)
    fsck_state = "not run" if not fsck_runs else "ok" if fsck_ok else "FAILED"
    print(f"\ngirt fsck: {fsck_state} over {len(fsck_runs)} runs")
    print(f"summary: {out}\nraw: {raw_out}")
    if failed_runs:
        print("FAILED runs (directories kept):", flush=True)
        for f in failed_runs:
            print(f"  {f}")
        sys.exit(1)


if __name__ == "__main__":
    main()
