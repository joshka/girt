"""Isolated adapter peak RSS with original large sources; no repository needed."""
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
EXE = ROOT / "target/release/examples" / ("ignore_batch.exe" if os.name == "nt" else "ignore_batch")
content = b"".join(f"generated-{n}.tmp\nmodule-{n}/**/*.cache\n".encode() for n in range(50_000))
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / "ignore"
    path.write_bytes(content)
    result = subprocess.run([str(EXE), str(path)], input=b"unmatched.rs\0", capture_output=True)
    assert result.returncode == 0, result.stderr
    assert not result.stdout
if os.name == "posix":
    import resource
    rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    rss_bytes = rss if platform.system() == "Darwin" else rss * 1024
else:
    rss_bytes = None
print(json.dumps({"platform": platform.platform(), "patterns": 100_000, "source_bytes": len(content), "child_peak_rss_bytes": rss_bytes, "scope": "one fresh adapter process; includes loader bytes, compiled set, query and startup; Windows RSS unavailable"}, indent=2))
