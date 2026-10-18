#!/usr/bin/env python3
"""Warm G/A/B/B/A/G history timing with a prebuilt harness and Git-written graph.

Run only after the coordinator gives the timing window. Preparation and builds must already be
complete. A is object-backed, B retains the default graph, G is Git. Both A and B include default
graph loading in opening; A removes the index before its query. Git log renders one line per commit,
whereas girt returns identities and prints a count. A Git rev-list count is an additional control.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", type=Path)
    parser.add_argument("harness", type=Path)
    parser.add_argument("tip")
    parser.add_argument("base")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--before", type=Path, help="compare indexed before/after harnesses")
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("rounds must be positive")
    repo, harness = args.repo.resolve(), args.harness.resolve()
    absent_config = Path("/tmp/girt-commit-graph-absent-global-config")
    assert not absent_config.exists(), "oracle config isolation requires an absent file"
    env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(absent_config), LC_ALL="C")
    git = ["git", "-C", str(repo), "-c", "core.commitGraph=true"]
    revision_range = f"{args.base}..{args.tip}"
    commands = {
        "range": {
            "G": git + ["log", "--oneline", "--no-decorate", revision_range],
            "A": [str(harness), str(repo), args.tip, args.base, "objects", "range"],
            "B": [str(harness), str(repo), args.tip, args.base, "indexed", "range"],
        },
        "ancestor": {
            "G": git + ["merge-base", "--is-ancestor", args.base, args.tip],
            "A": [str(harness), str(repo), args.tip, args.base, "objects", "ancestor"],
            "B": [str(harness), str(repo), args.tip, args.base, "indexed", "ancestor"],
        },
    }
    if args.before:
        before = args.before.resolve()
        for operation in commands:
            commands[operation]["A"] = [str(before), str(repo), args.tip, args.base, "indexed", operation]
    # Metadata and correctness controls are collected before the timed block.
    version = subprocess.check_output(git + ["--version"], env=env, text=True).strip()
    expected_count = int(subprocess.check_output(
        git + ["rev-list", "--count", revision_range], env=env))
    expected_ancestor = subprocess.run(commands["ancestor"]["G"], env=env).returncode
    if expected_ancestor not in (0, 1):
        raise SystemExit("Git ancestry control failed")
    graph = repo / "objects/info/commit-graph"
    metadata = {
        "protocol": "G A B B A G", "rounds": args.rounds, "git": version,
        "repo": str(repo), "tip": args.tip, "base": args.base,
        "harness_sha256": hashlib.sha256(harness.read_bytes()).hexdigest(),
        "graph_sha256": hashlib.sha256(graph.read_bytes()).hexdigest(),
        "range_count": expected_count, "ancestor": expected_ancestor == 0,
    }
    if args.before:
        metadata["before_harness_sha256"] = hashlib.sha256(args.before.read_bytes()).hexdigest()
        metadata["A"] = "indexed before"
        metadata["B"] = "indexed after"
    if os.getloadavg()[0] >= 12:
        raise SystemExit("one-minute load is not below 12; no lock taken")
    lock = Path("/tmp/girt-timing.lock")
    acquired = subprocess.run(["shlock", "-f", str(lock), "-p", str(os.getpid())])
    if acquired.returncode:
        raise SystemExit("timing lock is busy")
    try:
        if os.getloadavg()[0] >= 12:
            raise SystemExit("load rose before timing; releasing lock")
        print(json.dumps(dict(metadata, load_start=os.getloadavg())), flush=True)
        rows = []
        for operation, variants in commands.items():
            # Warm executable and repository pages in the same order before recording.
            for label in "GAB":
                warm = subprocess.run(variants[label], env=env, capture_output=True)
                expected_exit = expected_ancestor if operation == "ancestor" and label == "G" else 0
                assert warm.returncode == expected_exit, warm.stderr
            for round_number in range(args.rounds):
                for slot, label in enumerate("GABBAG"):
                    selected = [(label, variants[label])]
                    if label == "G" and operation == "range":
                        selected.append(("G-count", git + ["rev-list", "--count", revision_range]))
                    for implementation, command in selected:
                        started = time.perf_counter()
                        result = subprocess.run(["/usr/bin/time", "-l", *command],
                                                env=env, capture_output=True)
                        elapsed = time.perf_counter() - started
                        output = result.stdout.decode().strip()
                        if operation == "range":
                            observed = len(output.splitlines()) if implementation == "G" else int(output)
                            assert observed == expected_count, (command, observed, expected_count)
                            assert result.returncode == 0, result.stderr
                        elif implementation == "G":
                            assert result.returncode == expected_ancestor, result.stderr
                        else:
                            assert result.returncode == 0, result.stderr
                            assert output == str(expected_ancestor == 0).lower(), output
                        rss = re.search(rb"(\d+)\s+maximum resident set size", result.stderr)
                        assert rss, result.stderr
                        row = {"operation": operation, "implementation": implementation,
                               "round": round_number, "slot": slot, "seconds": elapsed,
                               "peak_rss_bytes": int(rss[1]), "command": command,
                               "stderr": result.stderr.decode(), "load": os.getloadavg()}
                        rows.append(row)
                        print(json.dumps(row), flush=True)
        for operation in commands:
            for implementation in ("G", "G-count", "A", "B"):
                selected = [r for r in rows if r["operation"] == operation
                            and r["implementation"] == implementation]
                if selected:
                    print(json.dumps({"summary": operation, "implementation": implementation,
                                      "median_seconds": statistics.median(r["seconds"] for r in selected),
                                      "peak_rss_bytes": max(r["peak_rss_bytes"] for r in selected)}), flush=True)
        print(json.dumps({"load_end": os.getloadavg()}), flush=True)
    finally:
        lock.unlink()


if __name__ == "__main__":
    main()
