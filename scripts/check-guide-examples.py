#!/usr/bin/env python3
"""Compile and run the standalone Rust programs in girt's developer guides.

Use Cargo's artifact paths so the examples use the same compiler dependencies as the library,
including custom target directories. Website content is excluded from published packages; an
archive without these guides has nothing to check.
"""

import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
GUIDES = ROOT / "site/src/content/docs/guides"
PAGES = ("index-and-working-tree.md", "diffs-and-renames.md", "maintenance.md")


def main():
    pages = [GUIDES / name for name in PAGES if (GUIDES / name).is_file()]
    if not pages:
        print("No developer website guides in this checkout.")
        return
    result = subprocess.run(
        ["cargo", "build", "--lib", "--all-features", "--message-format=json"],
        cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE,
    )
    libraries = {}
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact":
            continue
        name = message["target"]["name"]
        if name in ("girt", "tempfile"):
            for filename in message["filenames"]:
                if filename.endswith(".rlib"):
                    libraries[name] = Path(filename)
    externs = []
    for name in ("girt", "tempfile"):
        externs.extend(["--extern", f"{name}={libraries[name]}"])
    search_paths = []
    for directory in sorted({library.parent for library in libraries.values()}):
        search_paths.extend(["-L", f"dependency={directory}"])
    for page in pages:
        subprocess.run(
            [os.environ.get("RUSTDOC", "rustdoc"), "--test", "--edition=2024",
             str(page), *search_paths, *externs],
            cwd=ROOT, check=True,
        )


if __name__ == "__main__":
    main()
