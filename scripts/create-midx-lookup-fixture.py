#!/usr/bin/env python3
"""Create 100 small packs and present/absent ID lists for midx_lookup_timing.

Usage: python3 scripts/create-midx-lookup-fixture.py /path/to/new-fixture.git
No timing is performed. Coordinate a quiet window before benchmarking the fixture.
"""

import os
import pathlib
import random
import subprocess
import sys

root = pathlib.Path(sys.argv[1]).absolute()
root.mkdir(parents=True, exist_ok=False)
env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(root / "absent-config"))


def git(*args, data=b""):
    return subprocess.run(
        ["git", *args], cwd=root, env=env, input=data, capture_output=True, check=True
    ).stdout.strip()


git("init", "--bare", "--template=", "--initial-branch=main", ".")
present = []
absent = []
for pack in range(100):
    batch = []
    for item in range(8):
        batch.append(git("hash-object", "-w", "--stdin", data=f"original present {pack}:{item}".encode()))
        absent.append(git("hash-object", "--stdin", data=f"original absent {pack}:{item}".encode()))
    git("pack-objects", "--window=0", "objects/pack/pack", data=b"\n".join(batch) + b"\n")
    present.extend(batch)
git("prune-packed")
git("multi-pack-index", "write")
random.Random(173).shuffle(present)
random.Random(173).shuffle(absent)
(root / "present-ids").write_bytes(b"\n".join(present) + b"\n")
(root / "absent-ids").write_bytes(b"\n".join(absent) + b"\n")
print(f"Created {root}: 100 packs, 800 present IDs and 800 absent IDs")
