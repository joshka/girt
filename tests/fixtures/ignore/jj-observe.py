"""Observe unchanged jj's source layering, parent pruning and tracked-file selection."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "JJ_"))}
observations = []
with tempfile.TemporaryDirectory() as directory:
    base = Path(directory)
    home = base / "home"
    home.mkdir()
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home), JJ_CONFIG=str(home / "jj.toml"), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(home / "gitconfig"), GIT_AUTHOR_NAME="Original", GIT_AUTHOR_EMAIL="original@example.com", GIT_COMMITTER_NAME="Original", GIT_COMMITTER_EMAIL="original@example.com")
    def run(args, root):
        p = subprocess.run(args, cwd=root, env=env, capture_output=True, timeout=60)
        assert p.returncode == 0, (args, p.stderr)
        return p.stdout
    for fmt in ["sha1", "sha256"]:
        root = base / fmt
        root.mkdir()
        run(["git", "init", "-q", "--initial-branch=main", "--object-format=" + fmt], root)
        (root / "tracked.log").write_bytes(b"tracked\n")
        run(["git", "add", "tracked.log"], root)
        run(["git", "-c", "commit.gpgsign=false", "commit", "-qm", "original ignore fixture"], root)
        global_file = base / (fmt + "-global")
        global_file.write_bytes(b"*.log\n*.tmp\n")
        run(["git", "config", "core.excludesFile", str(global_file)], root)
        (root / ".git/info/exclude").write_bytes(b"!info.log\n")
        (root / ".gitignore").write_bytes(b"!root.log\nblocked/\n!blocked/keep\n")
        (root / "nested").mkdir()
        (root / "nested/.gitignore").write_bytes(b"!deep.log\n")
        (root / "blocked").mkdir()
        cases = {"root.log": True, "info.log": True, "other.log": False, "other.tmp": False, "nested/deep.log": True, "nested/other.log": False, "blocked/keep": False, "tracked.log": True, "visible": True}
        for name in cases:
            (root / name).write_bytes(b"original fixture\n")
        run(["jj", "--config", "signing.behavior='drop'", "git", "init", "--colocate"], root)
        files = run(["jj", "--config", "signing.behavior='drop'", "file", "list"], root).decode().splitlines()
        for name, expected in cases.items():
            assert (name in files) == expected, (fmt, name, expected, files)
        observations.append({"format": fmt, "files": files, "expected": cases})
print(json.dumps({"jj": subprocess.check_output(["jj", "--version"]).decode().strip(), "observations": observations}, indent=2))
