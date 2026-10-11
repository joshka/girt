"""Original POSIX regression probes for fixture ownership and late forks."""
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("ssh_fixture", Path(__file__).with_name("server.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def dead(pid):
    result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    return not result.stdout.strip() or result.stdout.strip().startswith("Z")


def wait_for(predicate):
    end = time.monotonic() + 10
    while not predicate():
        if time.monotonic() >= end:
            raise AssertionError("process lifetime deadline expired")
        time.sleep(.01)


class Cleanup(unittest.TestCase):
    def test_fork_after_first_snapshot_is_killed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            program = """import os,subprocess,sys,time
from pathlib import Path
root=Path(sys.argv[1])
if os.fork()==0:
    (root/'ready').touch()
    while not (root/'fork').exists(): time.sleep(.001)
    child=subprocess.Popen(['sleep','60'],start_new_session=True)
    (root/'pid.tmp').write_text(str(child.pid))
    (root/'pid.tmp').rename(root/'pid')
    time.sleep(60)
else: time.sleep(60)
"""
            child = subprocess.Popen(["python3", "-c", program, directory])
            original = subprocess.check_output
            first = True

            def snapshot(*args, **kwargs):
                nonlocal first
                listing = original(*args, **kwargs)
                if first:
                    first = False
                    (root / "fork").touch()
                    wait_for(lambda: (root / "pid").exists())
                return listing

            try:
                wait_for(lambda: (root / "ready").exists())
                with patch.object(fixture.subprocess, "check_output", snapshot):
                    fixture.stop_sshd(child)
                pid = int((root / "pid").read_text())
                wait_for(lambda: dead(pid))
            finally:
                if child.poll() is None:
                    fixture.stop_sshd(child)
                if (root / "pid").exists():
                    try:
                        os.kill(int((root / "pid").read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_listener_zombie_is_not_alive(self):
        owner = os.fork()
        if owner == 0:
            os._exit(0)
        try:
            wait_for(lambda: dead(owner))
            os.kill(owner, 0)  # PID existence alone would still report this owner alive.
            self.assertFalse(fixture.alive(owner))
        finally:
            os.waitpid(owner, 0)

    def test_http_owner_exit_before_reaping(self):
        self.owner_exit("http")

    def test_ssh_owner_exit_before_reaping(self):
        self.owner_exit("ssh")

    def test_proxy_owner_exit_before_reaping(self):
        self.owner_exit("proxy")

    def owner_exit(self, protocol):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = Path(__file__).parents[1]
            if protocol == "proxy":
                command = [str(fixtures / "http" / "connect_proxy.py"), "1", str(root / "requests")]
            else:
                command = [str(fixtures / protocol / "server.py"), directory]
            # This runner intentionally retains both the zombie owner and the stdin writer.
            owner = os.fork()
            if owner == 0:
                with (root / "output").open("w") as output:
                    child = subprocess.Popen(["python3", *command,
                                              "--owner", str(os.getpid())],
                                             stdin=read_fd, stdout=output, stderr=output)
                    (root / "pid.tmp").write_text(str(child.pid))
                    (root / "pid.tmp").rename(root / "pid")
                    time.sleep(60)
                os._exit(0)
            try:
                wait_for(lambda: (root / "pid").exists())
                pid = int((root / "pid").read_text())
                wait_for(lambda: (root / "output").stat().st_size > 0)
                output = (root / "output").read_text().strip()
                self.assertTrue(output.startswith("{") if protocol == "ssh" else output.isdecimal(), output)
                self.assertFalse(dead(pid), output)
                os.kill(owner, signal.SIGKILL)
                wait_for(lambda: dead(owner))
                wait_for(lambda: dead(pid))
            finally:
                try:
                    os.kill(owner, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(owner, 0)
                if (root / "pid").exists():
                    pid = int((root / "pid").read_text())
                    if not dead(pid):
                        os.kill(pid, signal.SIGKILL)


if __name__ == "__main__":
    read_fd, write_fd = os.pipe()
    try:
        unittest.main()
    finally:
        os.close(read_fd)
        os.close(write_fd)
