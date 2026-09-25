"""Original read/open observations using installed executables, never upstream source fixtures."""
import os
from pathlib import Path
import subprocess
import tempfile


def run(args, cwd, env):
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=30)


with tempfile.TemporaryDirectory(prefix="girt-r10-observe-") as temporary:
    root = Path(temporary)
    env = {key: value for key, value in os.environ.items() if not key.startswith(("GIT_", "JJ_"))}
    (root / "config.toml").write_text("")
    env.update(
        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(root / "absent"),
        GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.com",
        GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.com",
        GIT_AUTHOR_DATE="@1700000000 +0000", GIT_COMMITTER_DATE="@1700000000 +0000",
        JJ_CONFIG=str(root / "config.toml"), JJ_PAGER="cat",
    )
    for executable in ("git", "jj"):
        print(run([executable, "--version"], root, env).stdout.strip())
    for variable in ("GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE", "GIT_OBJECT_DIRECTORY", "GIT_SHALLOW_FILE"):
        fixture = root / variable
        fixture.mkdir()
        repository = fixture / "repo"
        result = run(["git", "init", "--quiet", "--template=", str(repository)], fixture, env)
        assert result.returncode == 0, result.stderr
        result = run(["git", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-m", "original fixture"], repository, env)
        assert result.returncode == 0, result.stderr
        observed_env = dict(env)
        observed_env[variable] = str(fixture / "nonexistent-override")
        args = ["jj", "--config", "signing.behavior='drop'", "--config", "user.name='Fixture'", "--config", "user.email='fixture@example.com'", "git", "init", "--git-repo", str(repository / ".git"), str(fixture / "workspace")]
        result = run(args, fixture, observed_env)
        print(variable, "explicit-path init exit", result.returncode)
        print(result.stderr.replace(temporary, "<fixture>").strip())
        if result.returncode == 0:
            result = run(["jj", "--ignore-working-copy", "log", "--no-graph", "-r", "all()", "-T", "description"], fixture / "workspace", observed_env)
            print("reopen exit", result.returncode, "imported fixture", "original fixture" in result.stdout)
            assert result.returncode == 0 and "original fixture" in result.stdout, result.stderr

    for object_format, width in (("sha1", 40), ("sha256", 64)):
        repository = root / object_format
        result = run(["git", "init", "--quiet", "--template=", f"--object-format={object_format}", str(repository)], root, env)
        assert result.returncode == 0, result.stderr
        result = run(["git", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-m", "shallow fixture"], repository, env)
        assert result.returncode == 0, result.stderr
        oid = run(["git", "rev-parse", "HEAD"], repository, env).stdout.strip().encode()
        cases = {
            "null": b"0" * width + b"\n", "blank": b"\n", "crlf": oid + b"\r\n",
            "unterminated": oid, "duplicate": oid + b"\n" + oid + b"\n",
            "missing": b"1" * width + b"\n", "wrong-format": b"1" * (104 - width) + b"\n",
        }
        for name, data in cases.items():
            (repository / ".git/shallow").write_bytes(data)
            result = run(["git", "rev-list", "--count", "HEAD"], repository, env)
            print(object_format, name, "exit", result.returncode, "count", result.stdout.strip(), "diagnostic", result.stderr.strip())

    for object_format in ("sha1", "sha256"):
        metadata = root / f"{object_format}-metadata"
        checkout = root / f"{object_format}-checkout"
        result = run(["git", "init", "--quiet", "--template=", f"--object-format={object_format}", f"--separate-git-dir={metadata}", str(checkout)], root, env)
        assert result.returncode == 0, result.stderr
        result = run(["jj", "--config", "signing.behavior='drop'", "git", "init", "--git-repo", str(metadata), str(root / f"{object_format}-workspace")], root, env)
        print(object_format, "jj separate metadata open exit", result.returncode)
        print(result.stderr.replace(temporary, "<fixture>").strip())
        assert result.returncode == 0, result.stderr
