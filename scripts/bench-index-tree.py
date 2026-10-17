#!/usr/bin/env python3
"""Compare release index-tree operations with Git on identical, uncached-tree indexes.

Build first: nice -n 10 cargo build --release --example index_tree_timing
Run: nice -n 10 python3 scripts/bench-index-tree.py --output /tmp/index-tree.json
The timing lock covers measurement only. Fixtures and binaries are prepared beforehand.
Each G A B B A G block restores the original index bytes (no TREE cache extension).
Use --cache-tree for the repeated-write comparison with a Git-generated TREE extension.
Objects and filesystem caches are warm; all three commands must produce the same root ID.
Per-run process wall time and macOS peak RSS are retained; API-only time excludes index decoding.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/examples/index_tree_timing")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--output", required=True)
    parser.add_argument("--cache-tree", action="store_true", help="retain Git-generated TREE extension")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    environment = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    report = {"git": run(["git", "--version"]).stdout.strip(),
              "rust": run(["rustc", "--version"]).stdout.strip(),
              "platform": run(["uname", "-a"]).stdout.strip(),
              "cache_tree": args.cache_tree, "cases": []}
    for count in [10_000, 100_000]:
        with tempfile.TemporaryDirectory(prefix="girt-index-tree-") as root:
            path = Path(root) / "repo"
            expected = run([binary, "fixture", str(path), str(count)], env=environment).stdout.strip()
            index = path / ".git" / "index"
            if args.cache_tree:
                assert run(["git", "-C", str(path), "write-tree"], env=environment).stdout.strip() == expected
            original = index.read_bytes()
            case = {"entries": count, "root": expected, "runs": [], "load_before": os.getloadavg()}
            lock = "/tmp/girt-timing.lock"
            while subprocess.run(["shlock", "-f", lock, "-p", str(os.getpid())], capture_output=True).returncode:
                time.sleep(1)
            try:
                commands = {"git": ["git", "-C", str(path), "write-tree"],
                            "tree-id": [binary, "tree-id", str(path)],
                            "checked": [binary, "checked", str(path)]}
                for _ in range(args.runs):
                    for name in ["git", "tree-id", "checked", "checked", "tree-id", "git"]:
                        index.write_bytes(original)
                        start = time.perf_counter()
                        result = run(["/usr/bin/time", "-l", *commands[name]], env=environment)
                        wall = time.perf_counter() - start
                        assert result.stdout.strip() == expected, (name, result.stdout, expected)
                        rss = re.search(r"(\d+)\s+maximum resident set size", result.stderr)
                        api = re.search(r"api_ns=(\d+)", result.stderr)
                        case["runs"].append({"command": name, "wall_s": wall,
                                             "rss_bytes": int(rss[1]) if rss else None,
                                             "api_s": int(api[1]) / 1e9 if api else None})
            finally:
                Path(lock).unlink()
            case["load_after"] = os.getloadavg()
            for name in ["git", "tree-id", "checked"]:
                values = [r for r in case["runs"] if r["command"] == name]
                best = min(values, key=lambda r: r["wall_s"])
                print(count, name, json.dumps(best), flush=True)
            report["cases"].append(case)
        # Temporary repository, index, objects and any cache extensions are deleted here.
    Path(args.output).write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
