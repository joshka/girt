#!/usr/bin/env python3
"""Original disposable loopback sshd fixture; no persistent account/configuration changes."""
import argparse
import getpass
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time


def faulty_upload_pack(original, env, fault):
    child = subprocess.Popen(["/bin/sh", "-c", original], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, env=env, start_new_session=True)
    def write_all(data):
        while data:
            data = data[os.write(1, data):]
    def copy_request():
        try:
            while data := os.read(0, 8192):
                child.stdin.write(data)
                child.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
        finally:
            try:
                child.stdin.close()
            except OSError:
                pass
    try:
        # Upload-pack advertises before the client sends wants. Relay that prefix live.
        while True:
            header = child.stdout.read(4)
            if len(header) != 4:
                return 1
            length = int(header, 16)
            payload = child.stdout.read(length - 4) if length else b""
            if len(payload) != max(length - 4, 0):
                return 1
            write_all(header + payload)
            if length == 0:
                break
        threading.Thread(target=copy_request, daemon=True).start()
        response = child.stdout.read()
        child.wait(timeout=20)
        if b"PACK" in response and fault == "fetch-corrupt-pack":
            pack = response.index(b"PACK")
            altered = bytearray(response)
            altered[pack + 12] ^= 1
            response = bytes(altered)
        elif b"PACK" in response:
            response = response[:-5]
        write_all(response)
        return 0
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


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
    if args.fault in ("fetch-corrupt-pack", "fetch-truncate") and words[0] == "git-upload-pack":
        return faulty_upload_pack(original, env, args.fault)
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
    owner = int(os.environ.get("GIRT_FIXTURE_OWNER", os.getppid()))
    stopped = False
    # Signal handlers must never raise in cleanup: a second signal used to orphan sshd.
    def stop_fixture(_signal, _frame):
        nonlocal stopped
        stopped = True
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, stop_fixture)
    with tempfile.TemporaryDirectory(prefix="girt-ssh-") as directory:
        root = Path(directory)
        for name in ("host", "client", "wrong"):
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
        authorized = "restrict " + (root / "client.pub").read_text()
        # Most tests need only an unencrypted key. Avoid paying bcrypt's passphrase KDF
        # in every concurrent fixture; the askpass test still exercises a real encrypted key.
        if args.encrypted_key:
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "fixture-passphrase",
                            "-f", str(root / "encrypted")], check=True)
            authorized += "restrict " + (root / "encrypted.pub").read_text()
        (root / "authorized_keys").write_text(authorized)
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
                                 ("encrypted_config", "encrypted", "known_hosts"),
                                 ("agent_config", "client.pub", "known_hosts"),
                                 ("unknown", "client", "empty_hosts"),
                                 ("changed", "client", "changed_hosts"),
                                 ("unauthorized", "wrong", "known_hosts")]:
            (root / name).write_text(f"IdentityFile {root}/{key}\nUserKnownHostsFile {root}/{trust}\nGlobalKnownHostsFile /dev/null\n")
        executable = shutil.which("sshd") or "/usr/sbin/sshd"
        log = (root / "diagnostics").open("w+")
        if stopped or os.getppid() != owner:
            return
        child = subprocess.Popen([executable, "-D", "-e", "-f", str(root / "sshd_config")], stderr=log)
        try:
            for _ in range(100):
                # Reserve the listener PID until tree cleanup, including startup failure.
                if os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):
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
            # EOF handles ordinary Drop; parent death also covers SIGKILL and inherited writers.
            while not stopped and os.getppid() == owner:
                readable, _, _ = select.select([0], [], [], .05)
                if readable and not os.read(0, 8192):
                    break
        finally:
            # sshd session children may create separate groups. Terminate the full owned process
            # tree before the leader. Freeze each parent before rescanning so it cannot fork or
            # reap descendants while we clean up. Our signal handlers cannot interrupt this.
            try:
                os.kill(child.pid, signal.SIGSTOP)
            except ProcessLookupError:
                pass
            owned = {child.pid}
            order = [child.pid]
            while True:
                listing = subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True)
                pairs = [tuple(map(int, line.split())) for line in listing.splitlines()]
                expanded = owned | {pid for pid, parent in pairs if parent in owned}
                if expanded == owned:
                    break
                for pid in expanded - owned:
                    try:
                        os.kill(pid, signal.SIGSTOP)
                    except ProcessLookupError:
                        pass
                    order.append(pid)
                owned = expanded
            # Children precede parents even if the OS wrapped its PID allocation counter.
            for pid in reversed(order[1:]):
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
    parser.add_argument("--encrypted-key", action="store_true")
    args = parser.parse_args()
    if args.serve:
        # Run subprocess cleanup even when SSH disconnects or the finite watchdog fires.
        def terminate(_signal, _frame):
            raise SystemExit(1)
        for signum in (signal.SIGHUP, signal.SIGTERM, signal.SIGALRM):
            signal.signal(signum, terminate)
        signal.alarm(25)
        sys.exit(service(args))
    main(args)
