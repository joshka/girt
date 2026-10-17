"""Original black-box lifetime regressions for the disposable SSH fixture.

Each test starts the real SSH fixture the way fixture_process::Process does (own process group,
stdin pipe held only by the owner, --owner <owner pid>), then removes the owner or the supervisor
in a specific order and asserts that every process under the fixture is gone within a bound.
"""
import ctypes
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

FIXTURE = Path(__file__).with_name("server.py")
BOUND = 8.0


def state(pid):
    out = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return out


def dead(pid):
    s = state(pid)
    return not s or s.startswith("Z")


def descendants(top):
    rows = [l.split() for l in subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True).splitlines()]
    owned = {top}
    while True:
        added = {int(p) for p, pp in rows if int(pp) in owned} - owned
        if not added:
            return owned
        owned |= added


def wait_for(predicate, bound=BOUND, what="condition"):
    end = time.monotonic() + bound
    while not predicate():
        if time.monotonic() >= end:
            raise AssertionError(f"{what} not met within {bound}s")
        time.sleep(0.02)


def port_refuses(port):
    try:
        with socket.create_connection(("127.0.0.1", port), 0.5):
            return False
    except OSError:
        return True


class Owner:
    """A forked stand-in for the Rust test process. Only it holds the fixture's stdin write end."""

    def __init__(self, root, fault="none", env=None):
        self.root = Path(root)
        expected_parent = os.getpid()
        self.pid = os.fork()
        if self.pid == 0:  # owner
            try:
                r, w = os.pipe()
                with (self.root / "out").open("w") as out:
                    child = subprocess.Popen([sys.executable, str(FIXTURE), str(self.root / "repo.git"),
                                              "--fault", fault, "--owner", str(os.getpid())],
                                             stdin=r, stdout=out, stderr=subprocess.DEVNULL,
                                             process_group=0, env=env)
                os.close(r)
                (self.root / "sup.tmp").write_text(str(child.pid))
                (self.root / "sup.tmp").rename(self.root / "sup")
                while os.getppid() == expected_parent:  # never close w while the harness owns us
                    time.sleep(.05)
            finally:
                os._exit(0)
        wait_for(lambda: (self.root / "sup").exists(), what="supervisor pid")
        self.supervisor = int((self.root / "sup").read_text())
        wait_for(lambda: (self.root / "out").exists() and (self.root / "out").stat().st_size > 0, what="readiness")
        import json
        info = json.loads((self.root / "out").read_text().splitlines()[0])
        self.port = info["port"]
        self.fixture_root = Path(info["root"])
        self.sshd = int((self.fixture_root / "pid").read_text())
        self.tree = descendants(self.supervisor)

    def kill_owner(self):
        os.kill(self.pid, signal.SIGKILL)
        os.waitpid(self.pid, 0)

    def finish(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        for pid in self.tree | descendants(self.supervisor):
            try:
                os.kill(pid, signal.SIGCONT)
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


class Cleanup(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="girt-fable-")
        self.root = Path(self.tmp.name)
        subprocess.run(["git", "init", "-q", "--bare", str(self.root / "repo.git")], check=True)

    def tearDown(self):
        self.tmp.cleanup()

    def assert_all_gone(self, owner):
        wait_for(lambda: all(dead(p) for p in owner.tree), what=f"tree {sorted(owner.tree)} gone")
        wait_for(lambda: port_refuses(owner.port), what="port closed")
        self.assertTrue(dead(owner.sshd))

    def test_supervisor_then_owner_sigkill(self):
        """Astra P1: SIGKILL the Python supervisor, then the owner. sshd must still stop."""
        owner = Owner(self.root)
        try:
            self.assertFalse(dead(owner.sshd))
            os.kill(owner.supervisor, signal.SIGKILL)
            time.sleep(0.3)
            owner.kill_owner()
            self.assert_all_gone(owner)
        finally:
            owner.finish()

    def test_owner_sigkill_while_supervisor_stopped(self):
        """Owner dies while the supervisor and listener are SIGSTOPped (a Drop in progress).
        Run under a same-session subreaper on Linux so the orphaned-group rule cannot help."""
        if sys.platform == "linux":
            libc = ctypes.CDLL(None, use_errno=True)
            self.assertEqual(libc.prctl(36, 1, 0, 0, 0), 0)  # PR_SET_CHILD_SUBREAPER
        owner = Owner(self.root)
        try:
            os.kill(owner.supervisor, signal.SIGSTOP)
            os.kill(owner.sshd, signal.SIGSTOP)
            wait_for(lambda: state(owner.supervisor).startswith("T"), what="supervisor stopped")
            owner.kill_owner()
            self.assert_all_gone(owner)
        finally:
            owner.finish()
            if sys.platform == "linux":
                libc.prctl(36, 0, 0, 0, 0)

    def test_sigterm_at_popen_boundary_in_forced_command(self):
        self.registration("none")

    def test_sigterm_at_popen_boundary_in_faulty_upload(self):
        self.registration("fetch-truncate")

    def registration(self, fault):
        """Astra P1: SIGTERM is delivered after Git's Popen has started the child but before
        the forced command owns it. Runs the --serve path directly (no sshd) under runpy with
        subprocess.Popen wrapped to signal itself before returning, so the fixture's own
        handler runs at that exact boundary."""
        shim = self.root / "bin"
        shim.mkdir()
        (shim / "git-upload-pack").write_text("#!/bin/sh\nexec sleep 60\n")
        (shim / "git-upload-pack").chmod(0o755)
        repo = str(self.root / "repo.git")
        # Take the exact --serve argv (including any guard arguments) from a live fixture.
        env = dict(os.environ)
        env["PATH"] = str(shim) + os.pathsep + env["PATH"]
        owner = Owner(self.root, fault=fault, env=env)
        import shlex
        force = next(l for l in (owner.fixture_root / "sshd_config").read_text().splitlines()
                     if l.startswith("ForceCommand "))
        argv = shlex.split(force[len("ForceCommand "):])
        runner = f"""
import os, runpy, signal, subprocess, sys, time
sys.argv = {argv[1:]!r}
os.environ["SSH_ORIGINAL_COMMAND"] = "git-upload-pack '" + {repo!r} + "'"
real = subprocess.Popen
class Popen(real):
    def __init__(self, args, *a, **k):
        super().__init__(args, *a, **k)
        if args and args[0] == "/bin/sh":
            from pathlib import Path
            # Pause with an empty staging file to reproduce the old open-before-write window.
            temporary = Path({str(self.root / 'git.pid.tmp')!r})
            with temporary.open("w") as output:
                Path({str(self.root / 'pid-open')!r}).touch()
                deadline = time.monotonic() + {BOUND}
                while not Path({str(self.root / 'publish-pid')!r}).exists():
                    if time.monotonic() >= deadline:
                        raise AssertionError("PID publication was not released")
                    time.sleep(.01)
                output.write(str(self.pid))
            temporary.replace(Path({str(self.root / 'git.pid')!r}))
            os.kill(os.getpid(), signal.SIGTERM)  # handled before __init__ returns
subprocess.Popen = Popen
runpy.run_path(sys.argv[0], run_name="__main__")
"""
        forced = subprocess.Popen([argv[0], "-c", runner], stdin=subprocess.PIPE, env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, process_group=0)
        try:
            wait_for(lambda: (self.root / "pid-open").exists(), what="PID staging file opened")
            self.assertEqual((self.root / "git.pid.tmp").read_text(), "")
            self.assertFalse((self.root / "git.pid").exists(), "incomplete PID was published")
            (self.root / "publish-pid").touch()
            wait_for(lambda: (self.root / "git.pid").exists(), what="git launched")
            git = int((self.root / "git.pid").read_text())
            wait_for(lambda: forced.poll() is not None, what="forced command exit")
            wait_for(lambda: dead(git), bound=5, what=f"git child {git} ({state(git)}) stopped after boundary signal")
        finally:
            forced.stdin.close()
            try:
                os.killpg(forced.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            forced.wait()
            forced.stderr.close()
            owner.finish()
            if (self.root / "git.pid").exists():
                try:
                    os.kill(int((self.root / "git.pid").read_text()), signal.SIGKILL)
                except (ProcessLookupError, ValueError):
                    pass

    def test_stalled_ps_cannot_hold_teardown(self):
        """Astra P2: a ps that blocks must not keep graceful teardown from finishing."""
        shim = self.root / "bin"
        shim.mkdir()
        pids = self.root / "ps.pids"
        (shim / "ps").write_text(f"#!/bin/sh\necho $$ >> {pids}\nexec sleep 30\n")
        (shim / "ps").chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = str(shim) + os.pathsep + env["PATH"]
        owner = Owner(self.root, env=env)
        try:
            os.kill(owner.supervisor, signal.SIGTERM)
            started = time.monotonic()
            wait_for(lambda: dead(owner.supervisor), bound=10, what="supervisor exit with stalled ps")
            self.assertLess(time.monotonic() - started, 8, "teardown exceeded its bound")
            self.assertTrue(dead(owner.sshd), "listener survived bounded teardown")
        finally:
            owner.finish()
            if pids.exists():  # only the shim processes this test started
                for pid in pids.read_text().split():
                    try:
                        os.kill(int(pid), signal.SIGKILL)
                    except (ProcessLookupError, ValueError):
                        pass


if __name__ == "__main__":
    unittest.main()
