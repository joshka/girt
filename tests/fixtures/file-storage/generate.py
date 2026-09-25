#!/usr/bin/env python3
"""Original Git CLI workload: 544 MiB uncompressed pack with bounded fixture-generation RAM."""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1]).resolve()
fmt = sys.argv[2] if len(sys.argv) > 2 else "sha1"
root.mkdir(parents=True, exist_ok=True)
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(root / "absent-config"))


def git(*args, data=None):
    return subprocess.check_output(["git", "-C", str(root), *args], input=data, env=env).strip()


git("init", "--bare", "--template=", "--initial-branch=main", f"--object-format={fmt}")
ids = []
for variant in range(17):
    process = subprocess.Popen(
        ["git", "-C", str(root), "-c", "core.compression=0", "hash-object", "-w", "--stdin"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env,
    )
    chunk = bytes([variant]) * (64 * 1024)
    for _ in range(512):
        process.stdin.write(chunk)
    process.stdin.close()
    ids.append(process.stdout.read().strip().decode())
    assert process.wait() == 0
small = git("hash-object", "-w", "--stdin", data=b"small lookup in a large store\n").decode()
ids.append(small)
# Disable delta search and compression to keep on-disk size above the old limit.
pack_hash = git(
    "-c", "core.compression=0", "pack-objects", "--window=0", "--depth=0",
    str(root / "objects/pack/pack"), data=("\n".join(ids) + "\n").encode(),
).decode()
git("prune-packed")
pair = root / "objects/pack" / f"pack-{pack_hash}"
report = {
    "format": fmt, "git": git("--version").decode(), "objects": len(ids),
    "large_object_bytes": 32 * 1024 * 1024, "small": small, "large": ids[0],
    "pack_bytes": pair.with_suffix(".pack").stat().st_size,
    "index_bytes": pair.with_suffix(".idx").stat().st_size,
}
(root / "workload.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report))
