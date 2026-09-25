#!/usr/bin/env python3
"""Run isolated readers; Linux cold runs request file-cache eviction, never promise physical I/O."""
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

root = Path(sys.argv[1]).resolve()
report = json.loads((root / "workload.json").read_text())
binary = Path("target/release/examples/storage_resources")
if os.name == "nt":
    binary = binary.with_suffix(".exe")
runs = []


def invoke(label, identity, mode=None, directory=root):
    command = [str(binary.resolve()), str(directory), identity]
    if mode:
        command.append(mode)
    if sys.platform == "darwin":
        command = ["/usr/bin/time", "-l", *command]
    elif sys.platform.startswith("linux"):
        command = ["/usr/bin/time", "-v", *command]
    result = subprocess.run(command, capture_output=True, text=True)
    record = dict(label=label, command=command, status=result.returncode,
                  stdout=result.stdout, stderr=result.stderr)
    runs.append(record)
    print(json.dumps(record), flush=True)
    Path("storage-measurements.json").write_text(json.dumps(
        dict(platform=platform.platform(), workload=report, runs=runs), indent=2
    ) + "\n")
    result.check_returncode()


# Warm opens still stream checksums; the process is fresh, while fixture generation warms the OS.
invoke("warm-os-small", report["small"])
invoke("warm-os-large-two-retained-results", report["large"])
invoke("asynchronous-open-cancellation", report["small"], "cancel")
if hasattr(os, "posix_fadvise") and hasattr(os, "POSIX_FADV_DONTNEED"):
    os.sync()
    for artifact in (root / "objects/pack").iterdir():
        with artifact.open("rb") as file:
            os.posix_fadvise(file.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
    invoke("cold-advised-small-no-physical-io-guarantee", report["small"])
    invoke("warm-after-advice-small", report["small"])

# The same large pack must also be usable through an alternate with the same shared bounds.
borrower = root / "borrower.git"
subprocess.run(["git", "init", "--bare", "--template=", "--initial-branch=main",
                f"--object-format={report['format']}", str(borrower)], check=True,
               capture_output=True)
(borrower / "objects/info/alternates").write_bytes(
    os.fsencode((root / "objects").as_posix()) + b"\n"
)
invoke("alternate-large-store-small", report["small"], directory=borrower)
