#!/usr/bin/env python3
"""Compare release store_open binaries and Git on fresh APFS clones, under the shared lock.

Build before invoking. Example (from the repository root):
  until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
  trap 'unlink /tmp/girt-timing.lock' EXIT
  python3 benches/store_open.py /tmp/opus-med-23/source.git \
    target/issue-61/store-open-main target/release/examples/store_open

The baseline binary must be built from the retained main revision with the same driver/toolchain.
Fixture copies and cleanup are outside each measured subprocess. Cache conditions are warm.
"""
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import time


def measure(source, binary, implementation, operation, run, destination):
    fixture = destination / f"fixture-{implementation}-{operation}-{run}.git"
    subprocess.run(["cp", "-cR", source, fixture], check=True)
    try:
        if implementation == "git":
            if operation == "repeat":
                command = ["sh", "-c", 'for n in 1 2 3 4 5 6 7 8; do git --git-dir="$1" cat-file -p HEAD >/dev/null || exit; done', "store-open", str(fixture)]
            else:
                command = ["git", f"--git-dir={fixture}", "cat-file", "-p", "HEAD"]
        else:
            command = [str(binary), operation, str(fixture)]
        started = time.perf_counter()
        output = subprocess.run(["/usr/bin/time", "-l", *command], capture_output=True, check=True, env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"})
        elapsed = time.perf_counter() - started
        diagnostics = output.stderr.decode()
        (destination / f"{implementation}-{operation}-{run}.time.txt").write_text(diagnostics)
        real = float(re.search(r"([\d.]+)\s+real", diagnostics)[1])
        rss = int(re.search(r"(\d+)\s+maximum resident set size", diagnostics)[1])
        phases = dict((name, float(value)) for name, value in re.findall(r"(\w+)_seconds=([\d.]+)", output.stdout.decode())) if implementation != "git" else {}
        return {"implementation": implementation, "operation": operation, "run": run, "wall_seconds": elapsed, "time_real_seconds": real, "peak_rss_bytes": rss, **phases}
    finally:
        shutil.rmtree(fixture)


def main():
    source, baseline, changed = map(lambda arg: Path(arg).resolve(), sys.argv[1:4])
    destination = Path("target/issue-61").resolve()
    destination.mkdir(parents=True, exist_ok=True)
    records = []
    # Warm executable pages as well as filesystem caches; retain the excluded warm-up records.
    for implementation in ["main", "girt", "git"]:
        warmup = measure(source, baseline if implementation == "main" else changed, implementation, "open", "warmup", destination)
        print(json.dumps(warmup), flush=True)
    for operation in ["open", "repeat", "refresh"]:
        for run in range(7):
            order = ["main", "girt", "git"] if run % 2 == 0 else ["git", "girt", "main"]
            for implementation in order:
                # Git has no explicit store-refresh operation; its row is the nearest read.
                record = measure(source, baseline if implementation == "main" else changed, implementation, operation, run, destination)
                records.append(record)
                print(json.dumps(record), flush=True)
    (destination / "measurements.json").write_text(json.dumps(records, indent=2) + "\n")
    for operation in ["open", "repeat", "refresh"]:
        for implementation in ["main", "girt", "git"]:
            rows = [record for record in records if record["operation"] == operation and record["implementation"] == implementation]
            wall = [row["wall_seconds"] for row in rows]
            print(operation, implementation, "wall median/range", statistics.median(wall), min(wall), max(wall), "peak RSS MiB", max(row["peak_rss_bytes"] for row in rows) / 1024 ** 2)
            if operation == "refresh" and implementation != "git":
                phase = [row["refresh"] for row in rows]
                print("refresh + HEAD phase median/range", statistics.median(phase), min(phase), max(phase))


if __name__ == "__main__":
    main()
