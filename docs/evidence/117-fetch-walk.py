#!/usr/bin/env python3
"""fetch-1 timing for #117: G A B B A G slots, each slot N samples, under the shared timing lock.

Templates (untimed, outside the lock): a copy of the bare origin plus a clone made by the tool and
one no-op fetch, as bench-ops does. Each sample copies its template with `cp -cR`, advances the
origin by one commit adding one blob (bench-ops' add_origin_commit), then times the fetch with
`/usr/bin/time -l`.
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

sys.path.insert(0, "/tmp/jj117")
JJ = {"A": "/tmp/jj117/t-a/release/jj", "B": "/tmp/jj117/t-b/release/jj"}
ENV = dict(os.environ, JJ_CONFIG="/dev/null", JJ_USER="bench", JJ_EMAIL="bench@example.com",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1", GIT_AUTHOR_NAME="bench",
           GIT_AUTHOR_EMAIL="bench@example.com", GIT_COMMITTER_NAME="bench",
           GIT_COMMITTER_EMAIL="bench@example.com")
SLOTS = "GABBAG"


def sh(cmd, cwd, extra=None):
    env = dict(ENV, **(extra or {}))
    return subprocess.run(cmd, shell=True, cwd=cwd, env=env, check=True, capture_output=True,
                          text=True).stdout.strip()


def add_origin_commit(origin, branch):
    index = {"GIT_INDEX_FILE": str(Path(origin) / "bench-index")}
    sh(f"git read-tree {branch}", origin, index)
    blob = sh(f"echo bench-{time.time_ns()} | git hash-object -w --stdin", origin)
    sh(f"git update-index --add --cacheinfo 100644,{blob},bench-fetch.txt", origin, index)
    tree = sh("git write-tree", origin, index)
    commit = sh(f"git commit-tree {tree} -p {branch} -m 'bench fetch'", origin)
    sh(f"git update-ref refs/heads/{branch} {commit}", origin)
    os.remove(index["GIT_INDEX_FILE"])


def fetch_cmd(tool):
    return "git fetch -q" if tool == "G" else f"{JJ[tool]} git fetch"


def template(work, origin, tool):
    t = work / f"template-{tool}"
    if t.exists():
        shutil.rmtree(t)
    t.mkdir(parents=True)
    subprocess.run(["cp", "-cR", origin, t / "origin.git"], check=True)
    if tool == "G":
        sh("git clone -q --no-local origin.git repo", t)
    else:
        sh(f"{JJ[tool]} git clone origin.git repo", t)
    sh(fetch_cmd(tool), t / "repo")
    return t


def sample(work, tool, branch, n):
    d = work / f"s-{tool}-{n}"
    if d.exists():
        shutil.rmtree(d)
    subprocess.run(["cp", "-cR", work / f"template-{tool}", d], check=True)
    repo = d / "repo"
    git_dir = repo / ".git" if (repo / ".git").is_dir() else repo / ".jj/repo/store/git"
    sh(f"git --git-dir={git_dir} config remote.origin.url {d / 'origin.git'}", d)
    add_origin_commit(d / "origin.git", branch)
    p = subprocess.run(f"/usr/bin/time -l {fetch_cmd(tool)}", shell=True, cwd=d / "repo",
                       env=ENV, capture_output=True, text=True)
    if p.returncode or "Nothing changed" in p.stderr:
        raise SystemExit(f"{tool} failed: {p.stderr}")
    head = sh(f"git --git-dir={git_dir} rev-parse refs/remotes/origin/{branch}", d)
    if head != sh(f"git rev-parse {branch}", d / "origin.git"):
        raise SystemExit(f"{tool} did not fetch the new commit")
    real = rss = None
    for line in p.stderr.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1] == "real":
            real = float(parts[0])
        if line.strip().endswith("maximum resident set size"):
            rss = int(parts[0]) / 2**20
    shutil.rmtree(d)
    return real, rss


def main():
    mode, name, origin, per_slot = sys.argv[1], sys.argv[2], Path(sys.argv[3]), int(sys.argv[4])
    work = Path(f"/tmp/jj117/work-{name}")
    work.mkdir(parents=True, exist_ok=True)
    branch = sh("git symbolic-ref --short HEAD", origin)
    if mode == "prep":
        for tool in "GAB":
            template(work, origin, tool)
        return
    results = {t: [] for t in "GAB"}
    n = 0
    for tool in SLOTS:
        for _ in range(per_slot):
            n += 1
            results[tool].append(sample(work, tool, branch, n))
    print(json.dumps({"repo": name, "load": os.getloadavg(), "results": results}))


main()
