"""Original deterministic malformed/glob corpus; executable-only Git observations."""
import itertools
import json
import os
from pathlib import Path
import platform
import random
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[3]
EXE = ROOT / "target/release/examples" / ("ignore_batch.exe" if os.name == "nt" else "ignore_batch")
ENV = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
ENV["GIT_CONFIG_NOSYSTEM"] = "1"
ENV["LC_ALL"] = "C"
paths = [bytes(p) for n in range(1, 6) for p in itertools.product(b"ab/", repeat=n)]
paths = [p for p in paths if not p.startswith(b"/") and not p.endswith(b"/") and b"//" not in p]
paths += [b"x", b"#a", b"!a", b"a ", b"a\\", b"*", b"[", b"\xff", b"\xff/x"]
batch = b"\0".join(paths) + b"\0"
rng = random.Random(170010)
patterns = [b"a/**\\/b", b"**\\/a", b"a/***/b", b"a/****/b", b"a\\/b", b"[a/]", b"[!/]", b"[[:unknown:]]", b"[![:unknown:]]", b"[a-]", b"[\\]]", b"[!]]", b"[z-a]", b"a\0b\r\n", b"a\r\0b\n"]
patterns += [bytes(rng.choice(b"ab/*?[]!^\\ -") for _ in range(rng.randrange(1, 15))) for _ in range(400)]
failures = []
observations = []
host_path_differences = []
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    ENV["GIT_CONFIG_GLOBAL"] = str(root / "absent-config")
    subprocess.run(["git", "init", "-q", directory], env=ENV, check=True)
    for pattern in patterns:
        (root / ".gitignore").write_bytes(pattern + b"\n")
        git = subprocess.run(["git", "-c", "core.ignoreCase=false", "-c", "core.excludesFile=", "check-ignore", "--no-index", "-z", "--stdin"], cwd=root, env=ENV, input=batch, capture_output=True)
        girt = subprocess.run([str(EXE), ".gitignore"], cwd=root, env=ENV, input=batch, capture_output=True)
        assert git.returncode in (0, 1), git.stderr
        assert girt.returncode == 0, girt.stderr
        observation = {"pattern_hex": pattern.hex(), "git_hex": git.stdout.hex(), "girt_hex": girt.stdout.hex()}
        observations.append(observation)
        if git.stdout != girt.stdout:
            git_paths = set(git.stdout.split(b"\0"))
            girt_paths = set(girt.stdout.split(b"\0"))
            # Git for Windows applies native path handling to a terminal backslash. Our API
            # accepts slash-separated byte paths, where backslash is a literal byte. Preserve
            # these observations separately; they are not lexical matcher compatibility cases.
            if os.name == "nt" and git_paths.symmetric_difference(girt_paths) <= {b"a\\"}:
                host_path_differences.append(observation)
            else:
                failures.append(observation)
report = {"platform": platform.platform(), "git": subprocess.check_output(["git", "--version"]).decode().strip(), "seed": 170010, "path_count": len(paths), "patterns": len(patterns), "paths_hex": [p.hex() for p in paths], "failures": failures, "host_path_differences": host_path_differences, "observations": observations}
print(json.dumps(report, indent=2))
sys.exit(bool(failures))
