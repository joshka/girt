#!/usr/bin/env python3
"""Original disposable loopback sshd fixture; no persistent account/configuration changes."""
import argparse
import getpass
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


def service(args):
    # sshd always authenticates an OS identity, but only this temporary key and forced command
    # are authorized here. Validate the exact repository/service before invoking any shell.
    original = os.environ.get("SSH_ORIGINAL_COMMAND", "")
    words = shlex.split(original)
    if len(words) != 2 or words[0] not in ("git-upload-pack", "git-receive-pack") or words[1] != args.repository:
        return 126
    quoted = "'" + args.repository.replace("'", "'\\''") + "'"
    if original != words[0] + " " + quoted:
        return 126
    root = Path(args.root)
    with (root / "requests").open("a") as out:
        out.write(original + "\n")
    if args.fault == "before-advertisement":
        time.sleep(10)
        return 1
    env = {"PATH": args.path, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
    if args.fault == "malformed":
        os.write(1, b"ZZZZ")
        return 1
    if args.fault == "diagnostics":
        os.write(2, b"private diagnostic\n" * 100000)
    if args.fault in ("after-report", "partial-report", "exit-failure", "blocked-upload", "exit-stall"):
        advertisement = b"0" * 40 + b" capabilities^{}\x00report-status\n"
        os.write(1, f"{len(advertisement)+4:04x}".encode() + advertisement + b"0000")
        if args.fault == "blocked-upload":
            time.sleep(10)
            return 1
        # Consume everything before status so writes can finish. These explicit fault cases are
        # transport tests, not claimed as Git interoperability.
        while os.read(0, 65536):
            pass
        def packet(data):
            os.write(1, f"{len(data)+4:04x}".encode() + data)
        packet(b"unpack ok\n")
        packet(b"ok refs/heads/main\n")
        if args.fault != "partial-report":
            os.write(1, b"0000")
        if args.fault == "exit-failure":
            return 42
        (root / "report").touch()
        if args.fault == "exit-stall":
            os.close(1)
        time.sleep(10)
        return 1
    child = subprocess.Popen(["/bin/sh", "-c", original], env=env, start_new_session=True)
    try:
        return child.wait(timeout=20)
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def main(args):
    with tempfile.TemporaryDirectory(prefix="girt-ssh-") as directory:
        root = Path(directory)
        for name in ("host", "client", "wrong"):
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
        (root / "authorized_keys").write_text("restrict " + (root / "client.pub").read_text())
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        command = shlex.join([sys.executable, str(Path(__file__).resolve()), args.repository,
                              "--serve", "--root", str(root), "--fault", args.fault,
                              "--path", os.environ["PATH"]])
        config = f"""ListenAddress 127.0.0.1
Port {port}
HostKey {root}/host
PidFile {root}/pid
AuthorizedKeysFile {root}/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers {getpass.getuser()}
AllowAgentForwarding no
AllowTcpForwarding no
X11Forwarding no
PermitTunnel no
PermitTTY no
ForceCommand {command}
LogLevel ERROR
"""
        (root / "sshd_config").write_text(config)
        host = (root / "host.pub").read_text()
        (root / "known_hosts").write_text(f"[127.0.0.1]:{port} " + host)
        (root / "empty_hosts").write_text("")
        (root / "changed_hosts").write_text(f"[127.0.0.1]:{port} " + (root / "wrong.pub").read_text())
        for name, key, trust in [("config", "client", "known_hosts"),
                                 ("unknown", "client", "empty_hosts"),
                                 ("changed", "client", "changed_hosts"),
                                 ("unauthorized", "wrong", "known_hosts")]:
            (root / name).write_text(f"IdentityFile {root}/{key}\nUserKnownHostsFile {root}/{trust}\nGlobalKnownHostsFile /dev/null\n")
        executable = shutil.which("sshd") or "/usr/sbin/sshd"
        log = (root / "diagnostics").open("w+")
        child = subprocess.Popen([executable, "-D", "-e", "-f", str(root / "sshd_config")], stderr=log)
        try:
            for _ in range(100):
                if child.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    with socket.create_connection(("127.0.0.1", port), .05):
                        break
                except OSError:
                    time.sleep(.02)
            else:
                raise RuntimeError("sshd did not listen on loopback")
            print(json.dumps({"port": port, "user": getpass.getuser(), "root": str(root)}), flush=True)
            sys.stdin.read()
        finally:
            # sshd session children may create separate groups. Terminate the full owned process
            # tree before the leader, using a fresh parent map while its PID remains reserved.
            listing = subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True)
            pairs = [tuple(map(int, line.split())) for line in listing.splitlines()]
            owned = {child.pid}
            while True:
                expanded = owned | {pid for pid, parent in pairs if parent in owned}
                if expanded == owned:
                    break
                owned = expanded
            for pid in sorted(owned - {child.pid}, reverse=True):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            child.kill()
            child.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("repository")
    parser.add_argument("--fault", default="none")
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--root")
    parser.add_argument("--path")
    args = parser.parse_args()
    if args.serve:
        # Run subprocess cleanup even when SSH disconnects or the finite watchdog fires.
        def terminate(_signal, _frame):
            raise SystemExit(1)
        for signum in (signal.SIGHUP, signal.SIGTERM, signal.SIGALRM):
            signal.signal(signum, terminate)
        signal.alarm(25)
        sys.exit(service(args))
    def stop_fixture(_signal, _frame):
        raise SystemExit(1)
    signal.signal(signal.SIGTERM, stop_fixture)
    main(args)
