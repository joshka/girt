#!/usr/bin/env python3
"""Original Git CLI observations of compaction and the separate indent heuristic.

Run from any directory. No upstream implementation or test inputs are used.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

CASES = {
    "merge_insertions": (b"b\n\n\nb\n", b"b\n\na\n\na\n\nb\n"),
    "align_replacement": (b"\na\n", b"a\na\n"),
    "merge_then_align": (b"b\na\n\na\na\n", b"b\nb\na\na\n"),
    "duplicate_block": (
        b"head\n  body\n}\n\nnext\n",
        b"head\n  body\n}\nhead\n  body\n}\n\nnext\n",
    ),
    "known_indent_gap": (
        b"prefix\n    cc\n\n  bb\n\n    cc\naa\n\n        dd\n",
        b"prefix\n    cc\n\n  bb\n\n    cc\naa\n\n        dd\n    cc\naa\n\n        dd\n",
    ),
    "blank_separator": (
        b"head\n\nhead\n  body\n}\n}\n\n\n}\n",
        b"head\n\nhead\n  body\nhead\n  body\n}\n}\n\n\n}\n",
    ),
}


def observe():
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, LC_ALL="C")
    result = {"git": subprocess.check_output(["git", "--version"], env=env).decode().strip()}
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        for name, (old, new) in CASES.items():
            (root / "old").write_bytes(old)
            (root / "new").write_bytes(new)
            observations = {}
            for flag in ("--no-indent-heuristic", "--indent-heuristic"):
                command = [
                    "git", "diff", "--no-index", "--text", "--minimal",
                    "--diff-algorithm=myers", "--no-ext-diff", "--no-textconv",
                    "--unified=0", flag, "--", "old", "new",
                ]
                output = subprocess.run(command, cwd=root, env=env, capture_output=True, check=False)
                assert output.returncode == 1, output.stderr
                # Hashes and temp paths are irrelevant; retain the independently observed hunks.
                patch = output.stdout.decode()
                observations[flag] = patch[patch.index("@@ "):]
            result[name] = observations
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    observe()
