"""Measure the original reftable resource probe in an isolated child process."""
import json
import platform
import subprocess
import sys
from pathlib import Path

executable = Path("target/release/examples/reftable_resources")
if platform.system() == "Windows":
    executable = executable.with_suffix(".exe")
result = subprocess.run([str(executable)], check=True, capture_output=True, text=True)
peak_bytes = None
if platform.system() != "Windows":
    import resource

    peak_bytes = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    if platform.system() != "Darwin":
        peak_bytes *= 1024
report = {
    "platform": platform.platform(),
    "workload": "SHA-1: initial HEAD, 16 updates of one tag, snapshot and full-stack compaction",
    "maximum_resident_bytes": peak_bytes,
    "probe_output": result.stdout,
    "limitations": "Whole-process peak; no cold-cache or many-reference memory claim. Windows RSS unavailable.",
}
Path(sys.argv[1] if len(sys.argv) > 1 else "reftable-resources.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report, indent=2))
