#!/usr/bin/env python3
"""G A B B A G comparison of jj status/log/new/one-commit fetch on git/git.

G = git (control), A = jj on girt main, B = jj on girt PR branch; stock jj is measured in each G
slot too. Every sample runs on a fresh `cp -c` copy of a prepared base; copying is untimed. The
whole timing block holds /tmp/girt-timing.lock.
"""
import json, os, re, shutil, subprocess, sys, time, statistics
W = "/tmp/b126/gabbag"
ORIGIN = "/tmp/b126/origin.git"
TOOLS = {
    "git": None,
    "stock": "/tmp/b119/jj-stock",
    "main": "/tmp/b126/jj-main",
    "new": "/tmp/b126/jj-new",
}
ENV = dict(os.environ, JJ_CONFIG="/dev/null", GIT_CONFIG_GLOBAL="/dev/null",
           GIT_CONFIG_NOSYSTEM="1", JJ_USER="b", JJ_EMAIL="b@example.com",
           GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@example.com",
           GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@example.com")
REPS = int(sys.argv[1]) if len(sys.argv) > 1 else 5

def sh(cmd, cwd):
    subprocess.run(cmd, shell=True, cwd=cwd, env=ENV, check=True, capture_output=True)

def git_dir(base, tool):
    if tool == "git" or os.path.isdir(f"{base}/repo/.git"):
        return f"{base}/repo/.git"
    return f"{base}/repo/.jj/repo/store/git"

def layout(base, tool):
    d = git_dir(base, tool)
    loose = sum(len(f) for _, _, f in os.walk(f"{d}/refs"))
    packed = 0
    if os.path.exists(f"{d}/packed-refs"):
        packed = sum(1 for l in open(f"{d}/packed-refs") if not l.startswith(("#", "^")))
    return dict(loose=loose, packed=packed)

def prepare(tool):
    base = f"{W}/base-{tool}"
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    subprocess.run(["cp", "-cR", ORIGIN, f"{base}/origin.git"], check=True)
    if tool == "git":
        sh("git clone -q --no-local origin.git repo", base)
    else:
        sh(f"{TOOLS[tool]} git clone origin.git repo", base)
        # One write command, as a user's first command after clone would be.
        sh(f"{TOOLS[tool]} new", f"{base}/repo")
    return base

def cmd(tool, op):
    if tool == "git":
        return {"status": "git status", "log": "git log --graph --oneline --decorate -n 20",
                "new": "git commit -q --allow-empty -m new", "fetch-1": "git fetch -q"}[op]
    jj = TOOLS[tool]
    return {"status": f"{jj} status", "log": f"{jj} log", "new": f"{jj} new",
            "fetch-1": f"{jj} git fetch"}[op]

def sample(tool, op, base):
    run = f"{W}/run"
    shutil.rmtree(run, ignore_errors=True)
    subprocess.run(["cp", "-cR", base, run], check=True)
    # Point the copy's remote at the copied origin.
    cfg = git_dir(run, tool) + "/config"
    text = open(cfg).read().replace(base, run)
    open(cfg, "w").write(text)
    if op == "fetch-1":
        o = f"{run}/origin.git"
        sh("git commit-tree -p refs/heads/master -m one $(git rev-parse refs/heads/master^{tree})"
           " | xargs git update-ref refs/heads/master", o)
    start = time.perf_counter()
    p = subprocess.run(["/usr/bin/time", "-l", "sh", "-c", cmd(tool, op)], cwd=f"{run}/repo",
                       env=ENV, capture_output=True, text=True)
    ms = (time.perf_counter() - start) * 1000
    if p.returncode:
        raise SystemExit(f"{tool} {op} failed: {p.stderr[-500:]}")
    rss = int(re.search(r"(\d+)\s+maximum resident set size", p.stderr).group(1)) / 1048576
    return ms, rss

def main():
    bases = {t: prepare(t) for t in TOOLS}
    layouts = {t: layout(b, t) for t, b in bases.items()}
    print(json.dumps(layouts), flush=True)
    while subprocess.run(["shlock", "-f", "/tmp/girt-timing.lock", "-p", str(os.getpid())]).returncode:
        time.sleep(15)
    records = []
    try:
        load = os.getloadavg()
        for slot in ["G", "A", "B", "B", "A", "G"]:
            tools = {"G": ["git", "stock"], "A": ["main"], "B": ["new"]}[slot]
            for tool in tools:
                for op in ["status", "log", "new", "fetch-1"]:
                    for _ in range(REPS):
                        ms, rss = sample(tool, op, bases[tool])
                        records.append(dict(slot=slot, tool=tool, op=op, ms=ms, rss=rss))
            print(slot, "done", flush=True)
        load_end = os.getloadavg()
    finally:
        os.remove("/tmp/girt-timing.lock")
    json.dump(dict(layouts=layouts, load_start=load, load_end=load_end, records=records),
              open(f"{W}/raw.json", "w"), indent=1)
    print(f"load {load} -> {load_end}")
    print("| op | git | stock jj | jj + girt main | jj + PR | PR/main | PR/stock |")
    print("|---|--:|--:|--:|--:|--:|--:|")
    for op in ["status", "log", "new", "fetch-1"]:
        med = {t: statistics.median(r["ms"] for r in records if r["tool"] == t and r["op"] == op)
               for t in TOOLS}
        rss = {t: max(r["rss"] for r in records if r["tool"] == t and r["op"] == op) for t in TOOLS}
        cell = lambda t: f"{med[t]:.1f} ms / {rss[t]:.0f} MiB"
        print(f"| {op} | {cell('git')} | {cell('stock')} | {cell('main')} | {cell('new')} |"
              f" {med['new']/med['main']:.2f}x | {med['new']/med['stock']:.2f}x |")

main()
