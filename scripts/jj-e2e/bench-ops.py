#!/usr/bin/env python3
"""Compares everyday jj operations on girt against stock jj and the closest Git command.

With --matrix, each operation is an independent cell. Prepare all copies and setup before acquiring
/tmp/girt-timing.lock, then measure Git, girt, stock, stock, girt, Git. Three blocks give six
samples per tool. With --adaptive --bounded-disk, calibrate in Git, stock, girt order,
then alternate girt/stock positions. Choose 1 sample if the slowest first sample exceeds 60 seconds,
3 if it exceeds 10 seconds, otherwise 6. Preserve completed older cells and partial samples.
Girt is killed at max(2 * first stock seconds, 5 seconds); stock and Git have 600-second caps.
Timeouts are failures, never successful timings. Flag stock/Git ratios above 2 as jj-level gaps.
Adaptive profiles stop after 5 seconds. Release the lock between blocks and operations. Mutating samples always use
fresh cp -cR copies; full Git fsck checks mutations according to --fsck-scope. Cells exceeding
1.2x stock also retain a samply profile and symbol sidecar. Matrix JSON contains medians, ranges,
peak RSS, binary fingerprints, exact revisions, ref layout, failures and semantic checks.

Matrix usage:
  bench-ops.py --matrix --girt-jj PATH --stock-jj PATH --girt-rev REV --jj-rev REV
               --stock-rev REV --repo small:NAME=BARE_REPO --repo medium:NAME=BARE_REPO
               --repo large:NAME=BARE_REPO --out ~/tmp/jj-bench-matrix.json

Without --matrix, the legacy sequential mode behaves as follows.
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
BENCH_HOME = Path.home() / "tmp" / "girt-bench"
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
        GIT_CONFIG_COUNT="2",
        GIT_CONFIG_KEY_0="maintenance.auto",
        GIT_CONFIG_VALUE_0="false",
        GIT_CONFIG_KEY_1="gc.auto",
        GIT_CONFIG_VALUE_1="0",
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


def timed(cmd, cwd, timeout=None):
    """Runs `cmd` under /usr/bin/time -l; returns wall ms, peak RSS MiB and status."""
    start = time.perf_counter()
    p = subprocess.Popen(["/usr/bin/time", "-l", "sh", "-c", cmd + " >/dev/null"], cwd=cwd,
                         env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                         start_new_session=True)
    expired = False
    try:
        _, stderr = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        expired = True
        os.killpg(p.pid, signal.SIGKILL)
        _, stderr = p.communicate()
    p.stderr_text = stderr
    ms = (time.perf_counter() - start) * 1000
    m = re.search(r"(\d+)\s+maximum resident set size", p.stderr_text)
    rss = int(m.group(1)) / 1048576 if m else 0.0
    return dict(ms=ms, rss_mb=rss, status=p.returncode, err=p.stderr_text,
                timeout=expired, timeout_limit_s=timeout)


def bounded_profile(command, cwd, log, seconds=None):
    """Bound the profiled process as well as samply's recording duration."""
    p = subprocess.Popen(command, cwd=cwd, env=ENV, stdout=subprocess.DEVNULL,
                         stderr=log, start_new_session=True)
    try:
        p.wait(timeout=seconds + 2 if seconds else None)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGINT)
        try:
            # Presymbolicating large release binaries can outlast the recording itself.
            p.wait(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.wait()
    return p.returncode


class Lock:
    """The machine-wide timing lock, held only while a run executes."""

    held = False

    def acquire(self):
        while True:
            # Other work inflates every tool's time; wait for a quieter machine before timing.
            waited = 0
            while MAX_LOAD is not None and (load := os.getloadavg()[0]) > MAX_LOAD:
                if waited % 300 == 0:
                    print(f'LOADWAIT load {load:.1f} > {MAX_LOAD:g}; waited {waited}s', flush=True)
                time.sleep(30)
                waited += 30
            if not subprocess.run(["shlock", "-f", LOCK, "-p", str(os.getpid())]).returncode:
                break
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
# One-minute load average above which timing waits; None disables the gate.
MAX_LOAD = None
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
        rustc=out(["rustc", "--version"]),
        cargo=out(["cargo", "--version"]),
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
            ("fetch-noop", "git fetch -q", "git fetch -q", repo),
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
        ("fetch-noop", f"{jj} git fetch", f"{jj} git fetch", repo),
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
    ap.add_argument("--work", default=str(BENCH_HOME / "legacy-run"))
    ap.add_argument("--out", default="~/tmp/jj-bench.json")
    ap.add_argument("--ops", default="", help="comma-separated operations to record")
    ap.add_argument("--girt-rev", default=None, help="girt revision the girt jj was built with")
    args = ap.parse_args()
    args.work = str(Path(args.work).expanduser().resolve())
    temp = Path(args.work) / "tmp"
    temp.mkdir(parents=True, exist_ok=True)
    ENV["TMPDIR"] = str(temp)
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
    def ratio(a, b):
        return f"{a / b:.2f}x" if a and b else "-"
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


# Comprehensive independent-cell mode. Legacy sequential mode remains available.
def matrix_ops(tool, binary, f):
    j = q(binary) if tool != 'git' else 'git'
    git = tool == 'git'
    ann = q(f['annotate'])
    big = f['diff_commit']
    dest = f['rebase_dest']
    base = f['head']
    # name, scenario, setup, command, equivalence/measurement note
    rows = []
    def add(name, scenario, setup, jj_cmd, git_cmd=None, note=''):
        rows.append(dict(op=name, scenario=scenario, setup=setup,
                         cmd=git_cmd if git else f'{j} {jj_cmd}', note=note))
    add('init-colocate', 'plain', '', 'git init --colocate', None,
        'Git has no equivalent: existing Git worktree already initialized')
    add('clone', 'empty', '', 'git clone origin.git repo',
        'git clone -q --no-local origin.git repo', 'local-path pack transfer; full checkout')
    add('import', 'clean', 'IMPORT', '--ignore-working-copy git import', None, 'import 100 external Git refs')
    add('export', 'clean', 'EXPORT', '--ignore-working-copy git export', 'git update-ref --stdin < ../export-refs.txt',
        'non-colocated export of 100 jj-only bookmarks; Git writes 100 pending refs in one transaction')
    for variant in ('noop', '1', '100', 'tags'):
        add('fetch-'+variant, 'clean', 'FETCH:'+variant, 'git fetch', 'git fetch -q',
            'untimed fetch imports existing remote state before delta; '
            + ('100 lightweight plus 20 annotated tags' if variant == 'tags' else variant+' new commits' if variant!='noop' else 'second fetch with no remote changes'))
    for variant in ('new', 'update', 'delete', 'stale'):
        add('push-'+variant, 'stack10', 'PUSH:'+variant,
            'git push -b matrix-push',
            'git push -q origin :refs/heads/matrix-push' if variant == 'delete' else
            'git push -q origin HEAD:refs/heads/matrix-push',
            'stale remote branch starts 1000 first-parent commits behind local base' if variant == 'stale' else '')
    for variant in ('clean', 'small-edit', 'many-edits', 'untracked', 'untracked-unique'):
        add('status-'+variant, 'clean', 'DIRTY:'+variant, 'status', 'git status',
            f'{min(1000, f["worktree_files"])} tracked files edited' if variant == 'many-edits' else
            '10000 identical-content untracked files across 100 directories; duplicate-blob workload' if variant == 'untracked' else
            '10000 unique-content untracked files across 100 directories; new-blob workload' if variant == 'untracked-unique' else '')
    for variant, opts in (('wc',''), ('stat','--stat'), ('git','--git')):
        add('diff-'+variant, 'dirty', '', 'diff '+opts,
            'git diff '+('--stat' if variant == 'stat' else ''), '100 tracked files edited')
    add('diff-large', 'clean', '', f'diff --git -r {big}', f'git diff {big}^ {big}',
        f'historical commit {big}, {f["diff_files"]} files changed')
    add('file-show', 'clean', '', f'file show {ann}', f'git show HEAD:{ann}')
    add('annotate', 'clean', '', f'file annotate {ann}', f'git blame -- {ann}')
    add('restore', 'dirty', '', 'restore', 'git restore .', '100 tracked files restored')
    for name, opts, gc in (
        ('default', '', 'git log --graph --oneline --decorate -n 20'),
        ('ancestry', "-r '::@'", 'git log --graph --oneline'),
        ('limited', "-r '::@' --limit 100", 'git log --graph --oneline -n 100'),
        ('no-graph', '--no-graph', 'git log --oneline -n 20'),
        ('large', "-r 'all()' --no-graph -T 'commit_id ++ \"\\n\"'", 'git log --all --format=%H')):
        add('log-'+name, 'clean', '', 'log '+opts, gc,
            'default revsets differ' if name in ('default','no-graph') else '100 revisions maximum from full ancestry; full output consumed to /dev/null' if name=='limited' else 'full output consumed to /dev/null')
    add('show', 'clean', '', f'show {big}', f'git show {big}')
    add('evolog', 'evolution', '', 'evolog -r @ --limit 100', 'git reflog show -n 100 HEAD',
        '30 description rewrites; Git HEAD reflog approximates this lineage with different metadata')
    add('op-log', 'evolution', '', 'op log --limit 100', 'git reflog -n 100',
        'operation history versus Git reflog: different records')
    add('new', 'clean', '', 'new', 'git commit -q --allow-empty -m new',
        'Git creates an empty commit; jj creates a working-copy commit')
    add('describe', 'stack10', '', 'describe -m matrix-described',
        'git commit -q --amend --allow-empty -m matrix-described')
    add('commit', 'dirty', '', 'commit -m matrix-commit', 'git commit -qam matrix-commit',
        'Git includes staging; jj includes snapshot and creates next working-copy commit')
    add('squash', 'stack10', '', 'squash -m matrix-squash',
        'git reset --soft HEAD~2 && git commit -qm matrix-squash')
    add('split', 'split', '', 'split -m matrix-split matrix-a.txt',
        'git reset -q HEAD~1 && git add matrix-a.txt && git commit -qm matrix-split && '
        'git add matrix-b.txt matrix-stack-0.txt && git commit -qm "matrix 0"',
        'noninteractive fileset split; Git stages and commits the same partition in two commits')
    add('abandon', 'stack10', '', 'abandon @', 'git reset --hard -q HEAD~1')
    add('edit', 'stack10', '', 'edit @-', 'git checkout -q HEAD~1')
    add('duplicate', 'stack10', '', 'duplicate @-',
        'git commit-tree HEAD~1^{tree} -p HEAD~2 -m matrix-duplicate | '
        'xargs git update-ref refs/heads/matrix-duplicate',
        'Git commit-tree plus update-ref preserves a sibling copy; Git description and identity differ')
    for n in (1, 10, 100):
        add('rebase-'+str(n), 'stack'+str(n), '',
            f'rebase -s {q(f"roots({base}..@)")} -d {dest}',
            f'git rebase -q --onto {dest} {base}', f'{n} nonempty commits moved backwards 20 main commits')
    add('rebase-bookmark', 'stack10', 'BOOKMARK',
        'rebase -b matrix-topic -d matrix-main', 'git rebase -q matrix-main',
        'whole 10-commit topic bookmark onto main advanced by one empty commit')
    add('undo', 'evolution', '', 'undo', None, 'Git has no operation undo equivalent')
    add('op-restore', 'evolution', '', 'op restore @-', None,
        'Git has no repository-operation restore equivalent')
    add('bookmark-list', 'clean', '', 'bookmark list --all-remotes', 'git branch -a')
    add('bookmark-create', 'clean', '', 'bookmark create matrix-new', 'git branch matrix-new')
    add('bookmark-move', 'clean', 'BM', 'bookmark set matrix-move -r @- --allow-backwards',
        'git branch -f matrix-move HEAD~1')
    add('bookmark-delete', 'clean', 'BM', 'bookmark delete matrix-move',
        'git branch -D matrix-move')
    add('gc-clean', 'clean', '', 'util gc', 'git gc -q',
        'no intentionally unreachable garbage; backend packing policies differ')
    add('gc-garbage', 'garbage', '', 'util gc --expire now', 'git gc -q --prune=now',
        '2000 unreachable loose blobs (8 MiB), aged 30 days; verify collection')
    add('workspace-add', 'clean', '', 'workspace add ../added',
        'git worktree add -q --detach ../added HEAD', 'full additional worktree checkout')
    return rows


def matrix_labels(case):
    """Expose operation and workload as separate JSON columns."""
    for prefix, operation in (('fetch-', 'git fetch'), ('push-', 'git push'),
                              ('status-', 'status'), ('diff-', 'diff'), ('log-', 'log'),
                              ('rebase-', 'rebase'), ('bookmark-', 'bookmark'),
                              ('gc-', 'util gc')):
        if case.startswith(prefix):
            return operation, case[len(prefix):]
    return {
        'init-colocate': ('git init', 'colocate'),
        'clone': ('git clone', 'local-full-checkout'),
        'import': ('git import', '100-external-refs'),
        'export': ('git export', '100-pending-bookmarks'),
        'file-show': ('file show', 'large-old-file'),
        'annotate': ('file annotate', 'large-old-file'),
        'workspace-add': ('workspace add', 'full-checkout'),
    }.get(case, (case.replace('-', ' '), 'default'))


def matrix_profile_summary(path):
    """Summarize retained samply stacks using its presymbolicated sidecar."""
    import gzip
    profile = json.loads(gzip.open(path, 'rt').read())
    symbols = json.loads(Path(str(path).removesuffix('.gz')+'.syms.json').read_text())
    strings = symbols['string_table']
    libraries = {lib['debug_name']: lib for lib in symbols['data']}
    for lib in libraries.values():
        lib['addresses'] = dict(lib['known_addresses'])
    inclusive, leaves = collections.Counter(), collections.Counter()
    main_inclusive = collections.Counter()
    count = 0
    for thread in profile['threads']:
        frame_names = []
        for i, address in enumerate(thread['frameTable']['address']):
            func = thread['frameTable']['func'][i]
            fallback = thread['stringArray'][thread['funcTable']['name'][func]]
            resource = thread['funcTable']['resource'][func]
            names = [fallback]
            if resource is not None and resource >= 0:
                index = thread['resourceTable']['lib'][resource]
                if index is not None:
                    lib = libraries.get(profile['libs'][index]['debugName'])
                    symbol = lib['addresses'].get(address) if lib else None
                    if symbol is not None:
                        entry = lib['symbol_table'][symbol]
                        names = [strings[f['function']] for f in entry.get('frames',[])
                                 if f.get('function') is not None]
                        if not names:
                            symbol_name = entry.get('symbol')
                            names = [strings[symbol_name]] if symbol_name is not None else [fallback]
            frame_names.append(names)
        stacks = thread['stackTable']
        for stack, weight in zip(thread['samples']['stack'],thread['samples']['weight']):
            if stack is None:
                continue
            names, leaf = set(), None
            while stack is not None:
                frames = frame_names[stacks['frame'][stack]]
                if leaf is None:
                    leaf = frames[0]
                names.update(frames)
                stack = stacks['prefix'][stack]
            count += weight
            leaves[leaf] += weight
            for name in names:
                inclusive[name] += weight
                if thread['isMainThread'] and thread['processName'].startswith('jj'):
                    main_inclusive[name] += weight
    result = dict(samples=count, inclusive=inclusive.most_common(50),
                  self_samples=leaves.most_common(30),
                  main_inclusive=main_inclusive.most_common(50),
                  girt_frames=[(k,v) for k,v in inclusive.most_common() if k.startswith(('girt::','<girt::'))][:60],
                  adapter_frames=[(k,v) for k,v in inclusive.most_common()
                                  if k.startswith(('jj_lib::git','<jj_lib::git'))][:60])
    Path(str(path).removesuffix('.json.gz')+'-summary.json').write_text(
        json.dumps(result,indent=2)+'\n')
    return result


def classify_matrix_limit(rec):
    """Keep explicit frozen-backend resource refusals out of successful timings."""
    if rec.get('status') and rec.get('tool') == 'girt':
        err = rec.get('err', '')
        if 'object storage limit exceeded: pack index bytes' in err:
            rec['unsupported_reason'] = 'Frozen girt fetch object snapshot exceeds pack-index byte limit'
        elif (rec.get('op', '').startswith('status-untracked')
              and 'rewrite candidates (' in err and 'exceed limit 1000' in err):
            rec['unsupported_reason'] = 'Frozen girt backend rewrite-candidate limit rejects this 10000-file tree'


def matrix_main():
    ap = argparse.ArgumentParser(description='Independent operation matrix, G A B B A G blocks')
    ap.add_argument('--matrix', action='store_true')
    ap.add_argument('--resume', action='store_true', help='resume checkpointed cells and baselines')
    ap.add_argument('--profile-only', action='store_true', help='profile selected completed cells without remeasurement')
    ap.add_argument('--profile-stock', action='store_true', help='also capture a matched stock-jj profile')
    ap.add_argument('--resume-rebuilt', action='store_true',
                    help='accept rebuilt binaries at identical source revisions; preserve old fingerprints')
    ap.add_argument('--girt-jj', required=True)
    ap.add_argument('--stock-jj', required=True)
    ap.add_argument('--repo', action='append', required=True, help='SIZE:NAME=BARE_REPO')
    ap.add_argument('--runs', type=int, default=6)
    ap.add_argument('--fsck-scope', choices=('all-cells','operation-type'), default='all-cells',
                    help='full fsck per cell, or once per variant plus clone/GC at every size')
    ap.add_argument('--adaptive', action='store_true',
                    help='calibrate Git/stock/girt; 6/3/1 samples at <=10/>10/>60 seconds; bounded timeouts')
    ap.add_argument('--bounded-disk', action='store_true',
                    help='use one per-run copy at a time; save and delete outputs after validation')
    ap.add_argument('--max-load', type=float, default=None,
                    help='before each timing-lock acquisition, wait until the one-minute load average is at or below this')
    ap.add_argument('--fresh-read-copies', action='store_true',
                    help='time read-only cells on a fresh copy per sample, like mutations')
    ap.add_argument('--work', default=str(BENCH_HOME / 'matrix-run'))
    ap.add_argument('--out', default='~/tmp/jj-bench-matrix.json')
    ap.add_argument('--ops', default='')
    ap.add_argument('--girt-rev', required=True)
    ap.add_argument('--jj-rev', required=True)
    ap.add_argument('--stock-rev', required=True)
    args = ap.parse_args()
    global MAX_LOAD
    MAX_LOAD = args.max_load
    if args.profile_only and (not args.resume or not args.ops):
        ap.error('--profile-only requires --resume and an explicit --ops list')
    if args.max_load is not None and args.max_load < 0:
        ap.error('--max-load must not be negative')
    if args.adaptive and not args.bounded_disk:
        ap.error('--adaptive requires --bounded-disk')
    if args.runs < 5:
        ap.error('at least five samples required (rounded up to even GAB BAG blocks)')
    out = Path(args.out).expanduser().resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    work = Path(args.work).expanduser().resolve()
    work.mkdir(parents=True, exist_ok=True)
    args.work = str(work)
    temp = work / "tmp"
    temp.mkdir(exist_ok=True)
    ENV["TMPDIR"] = str(temp)
    jj = {'girt': str(Path(args.girt_jj).resolve()),
          'stock': str(Path(args.stock_jj).resolve()), 'git': None}
    ENV.pop('JJ_RANDOMNESS_SEED', None)
    ENV.update(JJ_EDITOR='true', GIT_EDITOR='true', GIT_SEQUENCE_EDITOR='true',
               XDG_CONFIG_HOME=str(work/'xdg-config'), XDG_CACHE_HOME=str(work/'xdg-cache'),
               XDG_STATE_HOME=str(work/'xdg-state'))
    meta = provenance(args, jj)
    meta.update(jj_rev=args.jj_rev, stock_rev=args.stock_rev,
                order='git girt stock stock girt git', runs=args.runs,
                build_profile='release, debug=1, strip=none, codegen-units=1',
                read_samples='reuse separate read baseline; assert unchanged operation heads; mutation inputs remain pristine',
                automatic_git_maintenance='disabled; explicit GC operations still measured',
                adaptive=args.adaptive,
                adaptive_method='Git, stock, girt calibration; slowest first sample >60s:1, >10s:3, otherwise6; girt cap max(2*first stock,5s); stock/Git cap600s',
                fsck_scope=args.fsck_scope, max_load=args.max_load,
                large_disk_policy=('one fresh copy at a time; save timing/validation then rm -rf outside lock'
                                   if args.bounded_disk else 'retain complete GAB BAG block under timing lock'),
                harness_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    facts_all, records, checks, failures, skipped = {}, [], [], [], []
    profiles = []
    checkpoint = out.with_name(out.stem+'-raw.json')
    if args.resume and checkpoint.exists():
        old = json.loads(checkpoint.read_text())
        changed = any(old['provenance'][tool]['sha256'] != meta[tool]['sha256']
                      for tool in ('girt_jj','stock_jj'))
        if changed:
            if not args.resume_rebuilt:
                raise RuntimeError('resume binary fingerprint differs; use --resume-rebuilt for identical sources')
            for key in ('jj_rev','stock_rev'):
                if old['provenance'][key] != meta[key]:
                    raise RuntimeError('rebuilt binary source revision differs: '+key)
            if old['provenance']['girt_jj']['girt_rev'] != args.girt_rev:
                raise RuntimeError('rebuilt girt source revision differs')
            meta['rebuilt_after_restart'] = True
        records, checks = old['records'], old['fsck']
        for rec in records:
            rec.setdefault('binary_sha256', old['provenance'].get(rec['tool']+'_jj', {}).get('sha256'))
            rec.setdefault('run_epoch', old['provenance']['started'])
        for rec in records:
            if rec['op']=='status-untracked':
                rec['note']='10000 identical-content untracked files across 100 directories; duplicate-blob workload'
        failures, skipped, profiles = old['failures'], old['skipped'], old.get('profiles',[])
        for rec in records + failures:
            classify_matrix_limit(rec)
        for check in checks:
            if (check['status'] and check['op']=='clone'
                    and 'No such file or directory' in check.get('stderr','')
                    and any(r['repo']==check['repo'] and r['op']=='clone'
                            and r.get('unsupported_reason') for r in failures)):
                check['inapplicable_reason'] = 'Clone refused by backend limit; no result exists to fsck'
        meta['previous_provenance'] = old['provenance']
        facts_all = old['facts']
    only = set(filter(None, args.ops.split(',')))
    read_ops = {'file-show','annotate','show','evolog','op-log','bookmark-list','status-clean'}
    def needs_fsck(op):
        return (args.fsck_scope=='all-cells' or op in ('clone','gc-clean','gc-garbage')
                or not any(c['op']==op and c['status']==0 for c in checks))
    def write_json(path, value):
        pending = path.with_name(path.name+'.pending')
        pending.write_text(json.dumps(value, indent=2)+'\n')
        pending.replace(path)
    def save():
        rows = []
        for (repo, op), rs in __import__('itertools').groupby(
                sorted(records, key=lambda r: (r['repo'], r['op'])),
                key=lambda r: (r['repo'], r['op'])):
            rs = list(rs)
            operation, variant = matrix_labels(op)
            row = dict(repo=repo, size=facts_all[repo]['size'], op=operation, variant=variant)
            notes = [rs[0]['note']]
            for tool in TOOLS:
                rejected = [r for r in rs if r['tool']==tool and r.get('unsupported_reason')]
                if rejected:
                    notes.append(f'{tool} unsupported: {rejected[0]["unsupported_reason"]}; failed attempts={len(rejected)}; no successful timing')
            for tool in TOOLS:
                xs = [r for r in rs if r['tool'] == tool and r['status'] == 0]
                row[tool+'_samples'] = len(xs)
                row[tool+'_attempts'] = sum(r['tool']==tool for r in rs)
                row[tool+'_ms'] = statistics.median(r['ms'] for r in xs) if xs else None
                row[tool+'_rss_mb'] = max((r['rss_mb'] for r in xs), default=None)
                if xs:
                    notes.append(f'{tool} n={len(xs)} range={min(r["ms"] for r in xs):.3f}..{max(r["ms"] for r in xs):.3f}ms')
            row['ratio_vs_stock'] = row['girt_ms']/row['stock_ms'] if row['girt_ms'] and row['stock_ms'] else None
            row['sample_policy'] = 'adaptive' if any(r.get('adaptive') for r in rs) else 'fixed-six'
            if any(r.get('unsupported_reason') for r in rs):
                row['status'] = 'unsupported'
            timeouts = [r for r in rs if r.get('timeout')]
            if timeouts:
                row['status'] = 'timeout'
                row['timeout_limits_s'] = {r['tool']:r['timeout_limit_s'] for r in timeouts}
                if any(r['tool']=='girt' for r in timeouts):
                    row['girt_ms'] = row['ratio_vs_stock'] = None
                notes.append('timeout: '+str(row['timeout_limits_s']))
            row['stock_vs_git_ratio'] = row['stock_ms']/row['git_ms'] if row['stock_ms'] and row['git_ms'] else None
            row['jj_level_gap'] = bool(row['stock_vs_git_ratio'] and row['stock_vs_git_ratio']>2)
            row['notes'] = '; '.join(filter(None, notes))
            rows.append(row)
        write_json(out, rows)
        write_json(checkpoint, dict(provenance=meta, facts=facts_all, records=records,
                                    fsck=checks, failures=failures, skipped=skipped, profiles=profiles))
    def copy(src, dst):
        subprocess.run(['cp', '-cR', str(src), str(dst)], check=True)
    def dirty(repo, f, count):
        changed = 0
        for name in f['tracked']:
            if changed == count:
                break
            p = repo / name
            if p.is_file() and not p.is_symlink() and p.name != '.gitmodules':
                with p.open('ab') as stream:
                    stream.write(b'\nmatrix edit\n')
                changed += 1
        return changed
    def call(tool, cmd, repo):
        return sh(('git ' if tool == 'git' else q(jj[tool])+' ')+cmd, repo)
    def gd(repo):
        return repo / '.git'
    def prepare_scenario(tool, scenario, dst, clean, f):
        copy(clean, dst)
        repo = dst/'repo'
        if scenario.startswith('stack') or scenario == 'split':
            n = int(scenario[5:]) if scenario.startswith('stack') else 1
            for i in range(n):
                (repo / f'matrix-stack-{i}.txt').write_text(f'{i}\n')
                if scenario == 'split':
                    (repo/'matrix-a.txt').write_text('a\n')
                    (repo/'matrix-b.txt').write_text('b\n')
                if tool == 'git':
                    sh(f'git add . && git commit -qm "matrix {i}"', repo)
                else:
                    call(tool, f'describe -m "matrix {i}"', repo)
                    if i < n-1:
                        call(tool, 'new', repo)
        elif scenario == 'dirty':
            dirty(repo, f, 100)
        elif scenario == 'evolution':
            for i in range(30):
                call(tool, 'commit --amend --allow-empty -m evolution' if tool == 'git'
                     else f'describe -m evolution-{i}', repo)
        elif scenario == 'garbage':
            now = time.time()-30*86400
            payloads = dst/'garbage-payloads'
            payloads.mkdir()
            for i in range(2000):
                (payloads/str(i)).write_text(f'matrix garbage {i}\n'*240)
            paths = ''.join(str(payloads/str(i))+'\n' for i in range(2000))
            p = subprocess.run(['git','hash-object','-w','--stdin-paths'], cwd=repo,
                               env=ENV,input=paths,text=True,capture_output=True,check=True)
            for oid in p.stdout.splitlines():
                os.utime(gd(repo)/'objects'/oid[:2]/oid[2:], (now, now))
            shutil.rmtree(payloads)
        return dst
    def setup_cell(tool, spec, root, f):
        repo = root/'repo'
        action = spec['setup']
        if spec['scenario'] == 'empty':
            return root
        sh(f'git remote set-url origin {q(str(root/"origin.git"))}', repo)
        if tool == 'git' and spec['op'].startswith('rebase-'):
            # Linux contains distinct tracked paths that alias on case-insensitive APFS.
            # Ignore only these checkout artifacts, never genuine workload edits.
            groups = {}
            for path in f['tracked']:
                groups.setdefault(path.casefold(), []).append(path)
            aliases = {path for paths in groups.values() if len(paths) > 1 for path in paths}
            # Porcelain diff refreshes copied index stat data before reporting changes.
            changed = git_z(['diff', '--name-only', '-z'], repo)
            if set(changed) - aliases:
                raise RuntimeError('rebase input has changes beyond case-colliding paths')
            if changed:
                subprocess.run(['git', 'update-index', '--assume-unchanged', '-z', '--stdin'],
                               cwd=repo, env=ENV, check=True,
                               input=''.join(path+'\0' for path in changed), text=True)
                caveat = '; Git ignores case-colliding Linux checkout paths on APFS'
                if caveat not in spec['note']:
                    spec['note'] += caveat
        if not action:
            return repo
        if action == 'IMPORT':
            subprocess.run(['git','update-ref','--stdin'],cwd=repo,env=ENV,check=True,
                           input=''.join(f'update refs/heads/matrix-import-{i} {f["head"]}\n'
                                         for i in range(100)),text=True,capture_output=True)
        elif action == 'EXPORT':
            if tool=='git':
                (root/'export-refs.txt').write_text(''.join(
                    f'update refs/heads/matrix-export-{i} {f["head"]}\n' for i in range(100)))
                return repo
            # Non-colocated workspaces keep bookmark mutations pending until explicit export.
            hidden = repo/'.jj/repo/store/git'
            shutil.move(repo/'.git',hidden)
            (repo/'.jj/repo/store/git_target').write_text('git')
            sh(f'git --git-dir={q(str(hidden))} config core.bare true',repo)
            call(tool, 'bookmark create '
                 + ' '.join(f'matrix-export-{i}' for i in range(100)),repo)
        elif action.startswith('FETCH:'):
            variant = action.split(':')[1]
            # Import existing tags/remote state before introducing the measured delta.
            call(tool, 'fetch -q' if tool == 'git' else 'git fetch', repo)
            if variant in ('1','100'):
                for _ in range(int(variant)):
                    add_origin_commit(root/'origin.git', f['branch'])
            elif variant == 'tags':
                for i in range(100):
                    sh(f'git tag matrix-tag-{i} HEAD', root/'origin.git')
                for i in range(20):
                    sh(f'git tag -a matrix-ann-{i} -m tag HEAD', root/'origin.git')
        elif action.startswith('DIRTY:'):
            variant = action.split(':')[1]
            if variant == 'small-edit':
                dirty(repo, f, 1)
            elif variant == 'many-edits':
                dirty(repo, f, 1000)
            elif variant in ('untracked','untracked-unique'):
                for i in range(100):
                    d = repo/f'matrix-untracked/{i}'
                    d.mkdir(parents=True)
                    for k in range(100):
                        (d/str(k)).write_text(f'untracked {i}/{k}\n' if variant=='untracked-unique' else 'untracked\n')
        elif action.startswith('PUSH:'):
            variant = action.split(':')[1]
            if tool != 'git':
                call(tool, 'bookmark create matrix-push', repo)
            if variant != 'new':
                old = f['head']
                if variant == 'stale':
                    old = sh('git rev-parse HEAD~1000', root/'origin.git')
                sh(f'git update-ref refs/heads/matrix-push {old}', root/'origin.git')
                # Prepare remote-tracking state without repeating the separately measured
                # native fetch workload before every push sample.
                sh('git fetch -q', repo)
                if tool != 'git':
                    call(tool, 'git import', repo)
                    caveat = '; push prerequisite refs prepared by Git fetch plus jj import'
                    if caveat not in spec['note']:
                        spec['note'] += caveat
                    call(tool, 'bookmark track matrix-push@origin', repo)
                    if variant == 'delete':
                        call(tool, 'bookmark delete matrix-push', repo)
        elif action == 'BOOKMARK':
            tree = sh(f'git rev-parse {f["head"]}^{{tree}}',repo)
            commit = sh(f'git commit-tree {tree} -p {f["head"]} -m matrix-main',repo)
            sh(f'git update-ref refs/heads/matrix-main {commit}',repo)
            if tool != 'git':
                call(tool, 'git import',repo)
                call(tool, 'bookmark create matrix-topic -r @', repo)
        elif action == 'BM':
            call(tool, 'branch matrix-move' if tool == 'git' else 'bookmark create matrix-move', repo)
        return repo
    try:
        for spec in args.repo:
            label, origin = spec.split('=',1)
            size, name = label.split(':',1)
            origin = Path(origin).expanduser().resolve()
            f = facts(origin)
            tracked = git_z(['ls-tree','-rz','--name-only','HEAD'], origin)
            garbage = b'matrix garbage 0\n'*240
            f.update(size=size, tracked=tracked,
                     garbage_oid=hashlib.sha1(f'blob {len(garbage)}\0'.encode()+garbage).hexdigest(),
                     object_counts=sh('git count-objects -v', origin),
                     tags=int(sh('git tag -l | wc -l', origin)),
                     worktree_files=len(tracked),
                     annotate_bytes=int(sh(f'git cat-file -s HEAD:{q(f["annotate"])}',origin)),
                     pack_bytes=sum(p.stat().st_size for p in (origin/'objects/pack').glob('*.pack')),
                     loose_refs=sum(p.is_file() for p in (origin/'refs').rglob('*')),
                     packed_refs=sh("test ! -f packed-refs || awk '!/^#|^\\^|^$/ {n++} END {print n+0}' packed-refs", origin))
            if name in facts_all:
                f['checkout_ref_layout'] = facts_all[name].get('checkout_ref_layout',{})
            if args.profile_only:
                meta.setdefault('profiling_fixtures',{})[name] = f
            else:
                meta.setdefault('measurement_fixtures',{})[name] = f
                # Preserve original fixture facts when a durable same-HEAD repack is used.
                if name not in facts_all or facts_all[name]['head'] != f['head']:
                    facts_all[name] = f
            print(f'FACTS {name} '+json.dumps({k:v for k,v in f.items() if k != 'tracked'}), flush=True)
            baselines = {}
            specs = {t: matrix_ops(t,jj[t],f) for t in TOOLS}
            chosen = [s for s in specs['girt'] if not only or s['op'] in only]
            def setup_refused(tool, op):
                return any(r['repo']==name and r['op']==op and r['tool']==tool
                           and r.get('unsupported_phase')=='setup' for r in records)
            def cell_records(op, tool):
                return [r for r in records if r['repo']==name and r['op']==op and r['tool']==tool]
            def target_samples(op):
                if not args.adaptive:
                    return args.runs
                first = [next((r for r in cell_records(op,t) if r.get('ms') is not None), None)
                         for t in TOOLS if next(x for x in specs[t] if x['op']==op)['cmd']]
                if any(r is None for r in first):
                    return 6
                slowest = max(r['ms'] for r in first)
                return 1 if slowest>60000 else 3 if slowest>10000 else 6
            def tool_done(op, tool):
                rs = cell_records(op,tool)
                return (setup_refused(tool,op) or any(r.get('timeout') for r in rs)
                        or len([r for r in rs if r['status']==0 or r.get('unsupported_reason')])>=target_samples(op))
            def complete(cell):
                return all(tool_done(cell['op'],t) for t in TOOLS
                           if next(x for x in specs[t] if x['op']==cell['op'])['cmd'])
            if args.profile_only:
                if any(not complete(s) for s in chosen):
                    raise RuntimeError('profile-only requires completed measurements for every selected cell')
            elif args.resume:
                chosen = [s for s in chosen if not complete(s) or any(
                    p['repo']==name and p['op']==s['op'] and p['status'] for p in profiles)]
            if not chosen:
                continue
            common = work/f'{name}-plain-source'
            needed = set(s['scenario'] for s in chosen)-{'empty'}
            ready = work/f'{name}-baselines-ready.json'
            expected = dict(head=f['head'], scenarios=sorted(needed),
                            binaries={t:meta[t+'_jj']['sha256'] for t in ('girt','stock')})
            previous_ready = json.loads(ready.read_text()) if ready.exists() else {}
            reuse = (args.resume and previous_ready.get('head')==expected['head']
                     and previous_ready.get('binaries')==expected['binaries']
                     and set(previous_ready.get('scenarios',[])) >= needed
                     and common.exists() and all(
                (work/f'{name}-{t}-{scenario}').exists() for t in TOOLS for scenario in needed))
            if reuse:
                for t in TOOLS:
                    for scenario in needed:
                        baselines[(t,scenario)] = work/f'{name}-{t}-{scenario}'
            else:
                ready.unlink(missing_ok=True)
                if common.exists():
                    shutil.rmtree(common)
                common.mkdir()
                copy(origin,common/'origin.git')
                sh(f'git clone -q --local {q(str(common/"origin.git"))} repo',common)
                for tool in TOOLS:
                    print(f'PREPARE {name} {tool}', flush=True)
                    clean = work/f'{name}-{tool}-clean'
                    if clean.exists():
                        shutil.rmtree(clean)
                    copy(common,clean)
                    sh(f'git remote set-url origin {q(str(clean/"origin.git"))}',clean/'repo')
                    if tool != 'git':
                        call(tool, 'git init --colocate', clean/'repo')
                    baselines[(tool,'clean')] = clean
                    f.setdefault('checkout_ref_layout',{})[tool] = dict(
                        loose=sum(p.is_file() for p in (clean/'repo/.git/refs').rglob('*')),
                        packed=sh("test ! -f packed-refs || awk '!/^#|^\\^|^$/ {n++} END {print n+0}' packed-refs",clean/'repo/.git'))
                    for scenario in sorted(set(s['scenario'] for s in chosen)-{'clean','empty','plain'}):
                        dst = work/f'{name}-{tool}-{scenario}'
                        if dst.exists():
                            shutil.rmtree(dst)
                        baselines[(tool,scenario)] = prepare_scenario(tool,scenario,dst,clean,f)
                    plain = work/f'{name}-{tool}-plain'
                    if plain.exists():
                        shutil.rmtree(plain)
                    copy(clean,plain)
                    if (plain/'repo/.jj').exists():
                        shutil.rmtree(plain/'repo/.jj')
                    baselines[(tool,'plain')] = plain
                write_json(ready, expected)
            def read_baseline(tool, scenario):
                # Rendering may write synthesized Git trees/blobs without changing jj heads.
                # Keep those writes away from the pristine inputs used by mutation copies.
                key = (tool, scenario+'-readonly')
                if key not in baselines:
                    root = work/f'{name}-{tool}-{scenario}-readonly'
                    marker = root/'.matrix-read-ready.json'
                    identity = dict(head=f['head'], binary=meta.get(tool+'_jj',{}).get('sha256'))
                    ready_read = json.loads(marker.read_text()) if marker.exists() else None
                    if not args.resume or ready_read != identity:
                        if root.exists():
                            shutil.rmtree(root)
                        copy(baselines[(tool,scenario)],root)
                        write_json(marker,identity)
                    baselines[key] = root
                return baselines[key]

            for original in chosen:
                op = original['op']
                streamed = False
                single_copy = args.bounded_disk
                order = ('git','stock','girt','girt','stock','git') if args.adaptive else ('git','girt','stock','stock','girt','git')
                for iteration in range(0 if args.profile_only else (args.runs+1)//2 * (6 if single_copy else 1)):
                    if args.adaptive and complete(original):
                        break
                    block = iteration // 6 if single_copy else iteration
                    selected_position = iteration % 6 if single_copy else None
                    slots = [(position,t) for position,t in enumerate(order)
                             if (selected_position is None or position==selected_position)
                             and not setup_refused(t, op)
                             and not (args.adaptive and tool_done(op,t))
                             and next(x for x in specs[t] if x['op']==op)['cmd']]
                    if not args.adaptive and args.resume and all(any(r['repo']==name and r['op']==op and r['block']==block
                                               and r['position']==position and (r['status']==0 or bool(r.get('unsupported_reason')))
                                               for r in records) for position,t in slots):
                        continue
                    if not args.adaptive and args.resume:
                        records[:] = [r for r in records if not (
                            r['repo']==name and r['op']==op and r['block']==block
                            and any(r['position']==position for position,t in slots))]
                    pending = []
                    for position, tool in enumerate(order):
                        if (selected_position is not None and position!=selected_position) or setup_refused(tool, op) or (args.adaptive and tool_done(op,tool)):
                            continue
                        cell = next(s for s in specs[tool] if s['op'] == op)
                        if cell['cmd'] is None:
                            if block == 0 and not any(s['repo']==name and s['op']==op and s['tool']==tool for s in skipped):
                                skipped.append(dict(repo=name,op=op,tool=tool,reason=cell['note']))
                            continue
                        shared = (op.startswith('log-') or op in read_ops) and not args.fresh_read_copies
                        if shared:
                            root = read_baseline(tool,cell['scenario'])
                            cwd = setup_cell(tool,cell,root,f)
                            if tool!='git':
                                call(tool, 'config get user.name',cwd)
                            pending.append((tool,cell,root,cwd,position))
                            continue
                        root = work/f'cell-{name}-{op}-{block}-{position}'
                        if root.exists():
                            shutil.rmtree(root)
                        if cell['scenario'] == 'empty':
                            root.mkdir()
                            copy(origin,root/'origin.git')
                        else:
                            copy(baselines[(tool,cell['scenario'])],root)
                        try:
                            cwd = setup_cell(tool,cell,root,f)
                        except RuntimeError as error:
                            rec = dict(repo=name,size=size,op=op,tool=tool,block=block,
                                       position=position,cmd=cell['cmd'],note=cell['note'],
                                       ms=None,rss_mb=None,status=1,err=str(error),
                                       binary_sha256=meta.get(tool+'_jj',{}).get('sha256'),
                                       run_epoch=meta['started'],cwd=str(root/'repo'))
                            classify_matrix_limit(rec)
                            if not rec.get('unsupported_reason'):
                                raise
                            rec['unsupported_phase'] = 'setup'
                            rec['unsupported_reason'] += '; prerequisite failed before command timing'
                            records.append(rec)
                            failures.append(rec)
                            save()
                            subprocess.run(['rm', '-rf', str(root)], check=True)
                            continue
                        if tool!='git' and cell['scenario'] not in ('empty','plain'):
                            call(tool, 'config get user.name',cwd)
                        pending.append((tool,cell,root,cwd,position))
                    heads_before = {str(root): sorted(p.name for p in (root/'repo/.jj/repo/op_heads/heads').glob('*'))
                                    for tool,cell,root,cwd,position in pending
                                    if tool!='git' and (root in baselines.values()
                                                        or (args.fresh_read_copies
                                                            and (op.startswith('log-') or op in read_ops)))}
                    if not streamed:
                        LOCK_STATE.acquire()
                    try:
                        for tool,cell,root,cwd,position in pending:
                            if streamed:
                                LOCK_STATE.acquire()
                            try:
                                limit = None
                                if args.adaptive:
                                    stock_first = next((r['ms'] for r in cell_records(op,'stock') if r['status']==0), None)
                                    limit = max(2*stock_first/1000,5) if tool=='girt' and stock_first else 600
                                rec = timed(cell['cmd'],cwd,timeout=limit)
                                rec['adaptive'] = args.adaptive
                            finally:
                                if streamed:
                                    LOCK_STATE.release()
                            rec.update(repo=name,size=size,op=op,tool=tool,block=block,
                                       position=position,cmd=cell['cmd'],note=cell['note'],
                                       binary_sha256=meta.get(tool+'_jj',{}).get('sha256'),
                                       run_epoch=meta['started'],
                                       cwd=str(cwd),load=os.getloadavg())
                            records.append(rec)
                            print(f'{name} {op} {tool} {block}:{position} {rec["ms"]:.2f}ms {rec["rss_mb"]:.1f}MiB rc={rec["status"]}', flush=True)
                            if rec['status']:
                                classify_matrix_limit(rec)
                                failures.append(rec)
                            if streamed and not rec['status']:
                                rec['note'] += '; disk-heavy cell: release lock and discard output between GAB BAG samples'
                                if op=='gc-garbage':
                                    check = subprocess.run(['git','cat-file','-e',f['garbage_oid']],
                                        cwd=cwd,env=ENV,capture_output=True)
                                    rec['garbage_collected'] = check.returncode != 0
                                    if not rec['garbage_collected']:
                                        failures.append(dict(repo=name,op=op,tool=tool,
                                                             error='unreachable garbage retained'))
                                if block==0 and tool=='girt' and position==1 and needs_fsck(op):
                                    path = root/'repo/.git'
                                    p = subprocess.run(['git','-C',str(path),'fsck','--no-progress',
                                                        '--no-dangling'],env=ENV,capture_output=True,text=True)
                                    checks.append(dict(repo=name,op=op,path=str(path),
                                                       status=p.returncode,stderr=p.stderr[-2000:]))
                                shutil.rmtree(root)
                    finally:
                        if not streamed:
                            LOCK_STATE.release()
                    for tool,cell,root,cwd,position in pending:
                        if not root.exists() or any(r.get('timeout') and r.get('cwd')==str(cwd) for r in records):
                            continue
                        if str(root) in heads_before:
                            after = sorted(p.name for p in (root/'repo/.jj/repo/op_heads/heads').glob('*'))
                            if after != heads_before[str(root)]:
                                raise RuntimeError(f'read-only cell changed operation heads: {name} {op} {tool}')
                        if cell['op']=='export':
                            export_dir = '.git' if tool=='git' else '.jj/repo/store/git'
                            exported = sh(f"git --git-dir={export_dir} for-each-ref --format='%(refname)' 'refs/heads/matrix-export-*' | wc -l",cwd)
                            if int(exported)!=100:
                                failures.append(dict(repo=name,op=op,tool=tool,error='100 bookmarks not exported'))
                        if cell['op']=='import':
                            imported = call(tool, '--ignore-working-copy bookmark list',cwd)
                            if imported.count('matrix-import-')!=100:
                                failures.append(dict(repo=name,op=op,tool=tool,error='100 refs not imported'))
                        if cell['op']=='gc-garbage':
                            blob = subprocess.run(['git','hash-object','--stdin'],cwd=cwd,env=ENV,
                                input=b'matrix garbage 0\n'*240,capture_output=True,check=True).stdout.decode().strip()
                            check = subprocess.run(['git','cat-file','-e',blob],cwd=cwd,env=ENV,capture_output=True)
                            if check.returncode==0:
                                failures.append(dict(repo=name,op=op,tool=tool,error='unreachable garbage retained'))
                    # Full fsck at the selected scope, including push destinations when checked.
                    if block == 0 and not streamed and needs_fsck(op) and not (op.startswith('log-') or op in ('file-show','annotate','show','evolog','op-log','bookmark-list','status-clean')):
                        for tool,cell,root,cwd,position in pending:
                            if tool != 'girt' or any(r.get('timeout') and r.get('cwd')==str(cwd) for r in records):
                                continue
                            paths = [root/('repo/.jj/repo/store/git' if op=='export' else 'repo/.git')]
                            if op.startswith('push-'):
                                paths.append(root/'origin.git')
                            for path in paths:
                                p = subprocess.run(['git','-C',str(path),'fsck','--no-progress','--no-dangling'], env=ENV,capture_output=True,text=True)
                                checks.append(dict(repo=name,op=op,path=str(path),status=p.returncode,stderr=p.stderr[-2000:]))
                            break
                    save()
                    for tool,cell,root,cwd,position in pending:
                        if root.exists() and root not in baselines.values():
                            subprocess.run(['rm', '-rf', str(root)], check=True)
                times = {t: [r['ms'] for r in records if r['repo']==name and r['op']==op
                              and r['tool']==t and r['status']==0] for t in ('girt','stock')}
                profile_needed = (any(r.get('timeout') for r in cell_records(op,'girt'))
                                  or (times['girt'] and times['stock'] and statistics.median(times['girt']) > 1.2*statistics.median(times['stock'])))
                if profile_needed:
                    for profile_tool in (('girt','stock') if args.profile_stock else ('girt',)):
                        cell = next(s for s in specs[profile_tool] if s['op']==op)
                        shared_profile = (op.startswith('log-') or op in read_ops) and not args.fresh_read_copies
                        root = work/f'profile-{name}-{op}-{profile_tool}'
                        if shared_profile:
                            root = read_baseline(profile_tool,cell['scenario'])
                        elif root.exists():
                            shutil.rmtree(root)
                        if shared_profile:
                            pass
                        elif cell['scenario']=='empty':
                            root.mkdir()
                            copy(origin,root/'origin.git')
                        else:
                            copy(baselines[(profile_tool,cell['scenario'])],root)
                        cwd = setup_cell(profile_tool,cell,root,f)
                        if cell['scenario'] not in ('empty','plain'):
                            call(profile_tool,'config get user.name',cwd)
                        profile_dir = out.parent/'jj-matrix-profiles'
                        profile_dir.mkdir(exist_ok=True)
                        suffix = '-stock' if profile_tool=='stock' else ''
                        target = profile_dir/f'{name}-{op}{suffix}.json.gz'
                        LOCK_STATE.acquire()
                        try:
                            with target.with_suffix('.log').open('w') as log:
                                profile_status = bounded_profile([str(Path.home()/'.cargo/bin/samply'),'record','--save-only',
                                                    '--unstable-presymbolicate', *(['--duration','5'] if args.adaptive else []), '-o',str(target),
                                                    *shlex.split(cell['cmd'])],
                                                   cwd,log,seconds=5 if args.adaptive else None)
                        finally:
                            LOCK_STATE.release()
                        profile_saved = target.exists() and Path(str(target).removesuffix('.gz')+'.syms.json').exists()
                        if profile_saved:
                            matrix_profile_summary(target)
                        profiles[:] = [p for p in profiles if not (p['repo']==name and p['op']==op
                                                       and p.get('tool','girt')==profile_tool)]
                        profiles.append(dict(repo=name,op=op,tool=profile_tool,path=str(target),status=0 if profile_saved else profile_status,
                                             profiler_exit=profile_status, bounded_seconds=5 if args.adaptive else None,
                                             binary_sha256=meta[profile_tool+'_jj']['sha256'],
                                             command=cell['cmd']))
                        if not shared_profile:
                            shutil.rmtree(root)
                        save()
                print('DONE '+name+' '+op,flush=True)
            if not args.profile_only:
                ready.unlink(missing_ok=True)
                shutil.rmtree(common)
                for path in baselines.values():
                    if path.exists():
                        shutil.rmtree(path)
            save()
    finally:
        save()
    if any(not f.get('unsupported_reason') for f in failures) or any(c['status'] and not c.get('inapplicable_reason') for c in checks):
        sys.exit(1)


if __name__ == "__main__":
    matrix_main() if "--matrix" in sys.argv else main()
