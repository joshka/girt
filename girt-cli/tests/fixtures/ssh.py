"""Exercise the CLI against Git services; keys, sshd and repositories are disposable."""
import json
import os
from pathlib import Path
import select
import shlex
import subprocess
import sys
import tempfile


BINARY = str(Path(sys.argv[1]).resolve())
SERVER = Path(__file__).resolve().parents[3] / "tests/fixtures/ssh/server.py"


def run(args, cwd, env, success=True):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, input=b"", timeout=40)
    assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
    return result.stdout


def exercise(root, object_format):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_") and key not in ("SSH_AUTH_SOCK", "SSH_ASKPASS")}
    home = root / "home"
    home.mkdir()
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home), GIT_CONFIG_NOSYSTEM="1",
               GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.com",
               GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.com",
               TMPDIR=str(root))
    source = root / "source repo's.git"
    run(["git", "init", "--bare", "--initial-branch=main",
         f"--object-format={object_format}", str(source)], root, env)
    tree = run(["git", "mktree"], source, env).strip().decode()
    first = run(["git", "commit-tree", tree, "-m", "first"], source, env).strip().decode()
    run(["git", "update-ref", "refs/heads/main", first], source, env)
    run(["git", "config", "core.sshCommand", "false"], source, env)
    with tempfile.TemporaryFile(mode="w+") as diagnostics:
        server = subprocess.Popen(["python3", str(SERVER), str(source), "--owner", str(os.getpid())],
                                  env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=diagnostics, text=True)
        fixture_root = None
        try:
            assert select.select([server.stdout], [], [], 15)[0], "sshd readiness timeout"
            line = server.stdout.readline()
            if not line:
                diagnostics.seek(0)
                raise AssertionError(diagnostics.read())
            info = json.loads(line)
            fixture_root = Path(info["root"])
            config = fixture_root / "config"
            with config.open("a") as out:
                out.write(f"\nHost *\nPort {info['port']}\nUser {info['user']}\n"
                          "BatchMode yes\nStrictHostKeyChecking yes\nIdentitiesOnly yes\n")
            command = shlex.join(["/usr/bin/ssh", "-F", str(config)])
            run(["git", "config", "--global", "core.sshCommand", command], root, env)
            env["GIT_SSH_COMMAND"] = command
            url = f"ssh://{info['user']}@127.0.0.1:{info['port']}{source}"
            scp = f"{info['user']}@127.0.0.1:{source}"
            for index, address in enumerate((url, scp)):
                expected = run(["git", "ls-remote", address], root, env)
                assert run([BINARY, "remote", "heads", address], root, env) == expected
                destination = root / f"clone-{index}"
                global_config = dict(env)
                del global_config["GIT_SSH_COMMAND"]
                run([BINARY, "clone", address, str(destination)], source, global_config)
                assert run(["git", "rev-parse", "HEAD"], destination, env).strip().decode() == first
                assert run([BINARY, "remote", "heads", "origin"], destination, env) == expected
                # core.sshCommand works when the environment override is absent; a bad lower
                # priority command must not override GIT_SSH_COMMAND.
                run(["git", "config", "core.sshCommand", "false"], destination, env)
                assert run([BINARY, "remote", "heads", "origin"], destination, env) == expected
                run(["git", "config", "core.sshCommand", command], destination, env)
                configured = dict(env)
                del configured["GIT_SSH_COMMAND"]
                assert run([BINARY, "remote", "heads", "origin"], destination, configured) == expected
                fetched = run(["git", "commit-tree", tree, "-p", first, "-m", "remote"],
                              source, env).strip().decode()
                run(["git", "update-ref", "refs/heads/main", fetched], source, env)
                run([BINARY, "fetch"], destination, configured)
                assert run(["git", "rev-parse", "refs/remotes/origin/main"],
                           destination, env).strip().decode() == fetched
                second = run(["git", "commit-tree", tree, "-p", fetched, "-m", "second"],
                             destination, env).strip().decode()
                run(["git", "update-ref", "refs/heads/main", second], destination, env)
                run([BINARY, "push", "origin", "main"], destination, configured)
                assert run(["git", "rev-parse", "main"], source, env).strip().decode() == second
                run(["git", "fsck", "--strict"], source, env)
                run(["git", "update-ref", "refs/heads/main", first], source, env)
            # Host trust failure must propagate, with no automatic fallback.
            env["GIT_SSH_COMMAND"] = shlex.join(["/usr/bin/ssh", "-F", str(fixture_root / "unknown"),
                                                "-oBatchMode=yes", "-oStrictHostKeyChecking=yes"])
            run([BINARY, "remote", "heads", url], root, env, success=False)
        finally:
            body_error = sys.exc_info()[1]
            try:
                server.stdin.close()
                try:
                    status = server.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    server.terminate()
                    try:
                        server.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait(timeout=5)
                    raise AssertionError("SSH fixture cleanup timed out") from None
                assert status == 0, "SSH fixture cleanup failed"
                if fixture_root is not None:
                    assert not fixture_root.exists(), "SSH keys were not removed"
            except BaseException as cleanup_error:
                if body_error is None:
                    raise
                print(f"Additional cleanup failure: {cleanup_error}", file=sys.stderr)


scratch = Path.home() / "tmp"
scratch.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="girt-cli-ssh-", dir=scratch) as directory:
    for fmt in ("sha1", "sha256"):
        root = Path(directory) / fmt
        root.mkdir()
        exercise(root, fmt)
print("SSH clone, heads, fetch, push and host-trust refusal passed (SHA-1/SHA-256)")
