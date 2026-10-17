#!/usr/bin/env python3
"""Warm process timing on a repository, with prebuilt before/after binaries.

Usage: python3 benches/short_ids.py REPO BEFORE_DIR AFTER_DIR
Each binary directory contains girt and examples/short_ids. Build/copy before taking the lock.
Reports medians and peak RSS under /tmp/girt-timing.lock; retains individual samples as JSON lines.
"""
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time


def main():
    repo, before, after = [Path(arg).resolve() for arg in sys.argv[1:]]
    if subprocess.run(["shlock", "-f", "/tmp/girt-timing.lock", "-p", str(os.getpid())]).returncode:
        raise SystemExit("timing lock is busy")
    try:
        oid = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
        print(json.dumps({"load_start": os.getloadavg(), "head": oid}), flush=True)
        rows = []
        for operation in ["short", "log"]:
            commands = {
                "git": ["git", "-C", str(repo), "rev-parse", "--short", oid] if operation == "short"
                else ["git", "-C", str(repo), "log", "--oneline", "--no-decorate"],
            }
            for label, directory in [("before", before), ("after", after)]:
                commands[label] = [str(directory / "examples/short_ids"), str(repo), oid] if operation == "short" else [str(directory / "girt"), "-C", str(repo), "--git", "log", "--oneline"]
            # Warm executable pages and filesystem caches outside the recorded samples.
            expected = subprocess.check_output(commands["git"])
            for command in commands.values():
                assert subprocess.check_output(command) == expected, (operation, command)
            for run in range(5):
                for label in ["git", "before", "after", "after", "before", "git"]:
                    start = time.perf_counter()
                    result = subprocess.run(["/usr/bin/time", "-l", *commands[label]], capture_output=True, check=True)
                    elapsed = time.perf_counter() - start
                    rss = int(re.search(rb"(\d+)\s+maximum resident set size", result.stderr)[1])
                    row = {"operation": operation, "implementation": label, "run": run, "seconds": elapsed, "rss": rss}
                    rows.append(row)
                    print(json.dumps(row), flush=True)
            for label in commands:
                selected = [row for row in rows if row["operation"] == operation and row["implementation"] == label]
                print(json.dumps({"summary": operation, "implementation": label, "median_seconds": statistics.median(row["seconds"] for row in selected), "peak_rss": max(row["rss"] for row in selected)}), flush=True)
        print(json.dumps({"load_end": os.getloadavg()}), flush=True)
    finally:
        os.unlink("/tmp/girt-timing.lock")


if __name__ == "__main__":
    main()
