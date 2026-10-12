#!/usr/bin/env python3
"""Original disposable loopback sshd fixture; no persistent account/configuration changes."""
import argparse
from contextlib import contextmanager
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


CLEANUP_TIMEOUT = 2.0
GUARD_MARKER = "--lifetime-guard"
TERMINATION_SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM, signal.SIGALRM)


def scan(arguments, timeout):
    # Reserve the scan leader until its group is killed; a hung ps shim may itself fork.
    with tempfile.TemporaryFile(mode="w+") as output:
        child = subprocess.Popen(arguments, stdout=output, stderr=subprocess.DEVNULL,
                                 start_new_session=True)
        deadline = time.monotonic() + timeout
        try:
            while not os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):
                if time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(arguments, timeout)
                time.sleep(.001)
            output.seek(0)
            return output.read()
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                # macOS may reject signaling an already-exited, reserved scan group.
                pass
            child.wait(timeout=.2)


def alive(pid):
    # Non-parent watches reject zombies; a failed scan is unknown and retried next poll.
    try:
        state = scan(["ps", "-o", "stat=", "-p", str(pid)], .25).strip()
    except (OSError, subprocess.SubprocessError):
        return True
    return bool(state) and not state.startswith("Z")


def listing(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("fixture cleanup discovery deadline")
    output = scan(["ps", "-axo", "pid=,ppid=,pgid=,stat=,command="], remaining)
    return [line.split(None, 4) for line in output.splitlines()]


def kill_tree(roots, deadline, exclude=()):
    owned = set(roots) - set(exclude)
    order = list(owned)
    complete = False
    try:
        while time.monotonic() < deadline:
            for pid in owned:
                try:
                    os.kill(pid, signal.SIGSTOP)
                except ProcessLookupError:
                    pass
            rows = listing(deadline)
            added = {int(pid) for pid, parent, _, _, command in rows
                     if int(parent) in owned and GUARD_MARKER not in command} - owned - set(exclude)
            # Stop on discovery, before another scan can lose a forking parent.
            for pid in added:
                try:
                    os.kill(pid, signal.SIGSTOP)
                except ProcessLookupError:
                    pass
            order.extend(sorted(added))
            owned.update(added)
            running = any(int(pid) in owned and not state.startswith(("T", "t", "Z"))
                          for pid, _, _, state, _ in rows)
            if not added and not running:
                complete = True
                break
            time.sleep(.001)
    except (OSError, subprocess.SubprocessError, TimeoutError) as error:
        print(f"fixture cleanup discovery failed: {error}", file=sys.stderr)
    finally:
        for pid in reversed(order):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    return complete


def group_children(group, exclude, deadline):
    try:
        roots = [int(pid) for pid, _, pgid, _, command in listing(deadline)
                 if int(pgid) == group and int(pid) not in exclude and GUARD_MARKER not in command]
        if not kill_tree(roots, deadline, exclude):
            return False
        # SIGKILL is asynchronous. Wait for the killed members to exit before declaring normal
        # cleanup incomplete; an immediate snapshot can still show a doomed Git child running.
        while time.monotonic() < deadline:
            if not any(int(pgid) == group and int(pid) not in exclude
                       and GUARD_MARKER not in command and not state.startswith("Z")
                       for pid, _, pgid, state, command in listing(deadline)):
                return True
            time.sleep(.001)
        return False
    except (OSError, subprocess.SubprocessError, TimeoutError) as error:
        print(f"fixture group discovery failed: {error}", file=sys.stderr)
        return False


def lifetime_guard(args):
    # This group member reserves its ID, remains runnable during discovery, and has no SSH pipes.
    for signum in TERMINATION_SIGNALS:
        signal.signal(signum, signal.SIG_IGN)
    print("ready", flush=True)
    os.close(1)
    next_check = 0.0
    eof_deadline = None
    while True:
        readable, _, _ = select.select([0, args.lifetime_fd], [], [], .05)
        if 0 in readable:
            message = os.read(0, 16)
            if message == b"done":
                os._exit(0)
            if not message:
                break
        if args.lifetime_fd in readable and not os.read(args.lifetime_fd, 16):
            # Closing stdin also begins ordinary Drop. Give the primary cleanup a bounded chance
            # to send "done"; hard owner death is detected independently below.
            if eof_deadline is None:
                eof_deadline = time.monotonic() + CLEANUP_TIMEOUT
            if time.monotonic() >= eof_deadline:
                break
        if os.getppid() != args.owner:
            break
        if time.monotonic() >= next_check:
            next_check = time.monotonic() + .5
            if not alive(args.test_owner):
                break
    # Recover reachable SSH session groups before the final kernel group kill. The forced-command
    # guards cover their own group even if its parent was already reparented before this scan.
    group_children(args.group, {os.getpid()}, time.monotonic() + CLEANUP_TIMEOUT)
    os.killpg(args.group, signal.SIGKILL)
    os._exit(1)


class LifetimeGuard:
    def __init__(self, test_owner, lifetime_path=None):
        # Keep the existing session: Rust's fixture group must never use setsid. A forced command
        # gets its own group so the guard cannot kill sshd's exit-status parent.
        if os.getpgrp() != os.getpid():
            os.setpgid(0, 0)
        self.group = os.getpgrp()
        lifetime_fd = (os.open(lifetime_path, os.O_RDONLY | os.O_NONBLOCK)
                       if lifetime_path else os.dup(0))
        self.child = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), ".", GUARD_MARKER,
             "--owner", str(os.getpid()), "--test-owner", str(test_owner),
             "--lifetime-fd", str(lifetime_fd), "--group", str(self.group)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, pass_fds=(lifetime_fd,))
        os.close(lifetime_fd)
        if not select.select([self.child.stdout], [], [], 10)[0] or self.child.stdout.readline() != b"ready\n":
            raise RuntimeError("fixture lifetime guard did not start")
        self.child.stdout.close()

    def close(self):
        # Protected children finish first; the guard exits quietly before SSH delivers our status.
        complete = group_children(self.group, {os.getpid(), self.child.pid},
                                  time.monotonic() + CLEANUP_TIMEOUT)
        if not complete:
            # EOF arms the guard's group-kill fallback. Never announce a clean shutdown after
            # incomplete discovery, and leave the independent guard alive through our exit.
            print("fixture cleanup incomplete; retaining lifetime guard", file=sys.stderr)
            self.child.stdin.close()
            os._exit(1)
        try:
            self.child.stdin.write(b"done")
            self.child.stdin.flush()
            self.child.stdin.close()
            self.child.wait(timeout=CLEANUP_TIMEOUT)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            self.child.kill()
            self.child.wait(timeout=CLEANUP_TIMEOUT)


launching = False
stop_requested = False


@contextmanager
def owned_child(*arguments, **options):
    global launching, stop_requested
    child = None
    try:
        # Termination handlers record the request until the returned handle is registered.
        launching = True
        try:
            child = subprocess.Popen(*arguments, **options)
        finally:
            launching = False
        if stop_requested:
            stop(signal.SIGTERM, None)
        yield child
    finally:
        begin_cleanup()
        if child is not None and child.returncode is None:
            stop_sshd(child)


stopping = False


def stop(_signal, _frame):
    # The first termination signal raises SystemExit so `finally` cleanup runs. Later signals, or
    # any signal once cleanup has begun, must not interrupt it and strand the processes it stops.
    global stopping, stop_requested
    if launching:
        stop_requested = True
        return
    if not stopping:
        stopping = True
        raise SystemExit(1)


def begin_cleanup():
    global stopping
    stopping = True


def faulty_upload_pack(original, env, fault):
    with owned_child(["/bin/sh", "-c", original], stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, env=env) as child:
        return relay_upload_pack(child, fault)


def relay_upload_pack(child, fault):
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
        begin_cleanup()


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
    with owned_child(["/bin/sh", "-c", original], env=env) as child:
        return child.wait(timeout=20)


def main(args):
    with tempfile.TemporaryDirectory(prefix="girt-ssh-") as directory:
        root = Path(directory)
        lifetime = root / "lifetime"
        os.mkfifo(lifetime)
        # Only this supervisor holds the writer. Forced guards observe EOF even if a subreaper
        # adopts their stopped Python parents or numeric PIDs are later reused.
        lifetime_writer = os.open(lifetime, os.O_RDWR | os.O_NONBLOCK)
        for name in ("host", "client", "wrong"):
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
        authorized = "restrict " + (root / "client.pub").read_text()
        # Only the askpass case needs the expensive passphrase KDF.
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
                              "--path", os.environ["PATH"], "--owner", str(os.getpid()),
                              "--test-owner", str(args.owner)])
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
        with owned_child([executable, "-D", "-e", "-f", str(root / "sshd_config")],
                         stdin=subprocess.DEVNULL, stderr=log) as child:
            # sshd stays in this process group, so the test's group kill also stops the listener.
            for _ in range(100):
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
            # Stop when the test closes stdin or exits. A killed test cannot close stdin itself,
            # and another process may still hold the pipe, so also watch the owner directly.
            while os.getppid() == args.owner:
                if select.select([sys.stdin], [], [], 0.05)[0] and not os.read(0, 4096):
                    break
            begin_cleanup()

        os.close(lifetime_writer)


def stop_sshd(child):
    kill_tree([child.pid], time.monotonic() + CLEANUP_TIMEOUT)
    try:
        child.wait(timeout=CLEANUP_TIMEOUT)
    except subprocess.TimeoutExpired:
        print("fixture leader did not reap within cleanup deadline", file=sys.stderr)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("repository")
    parser.add_argument(GUARD_MARKER, action="store_true")
    parser.add_argument("--group", type=int)
    parser.add_argument("--test-owner", type=int)
    parser.add_argument("--lifetime-fd", type=int)
    parser.add_argument("--fault", default="none")
    parser.add_argument("--encrypted-key", action="store_true")
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--root")
    parser.add_argument("--path")
    parser.add_argument("--owner", type=int, required=True,
                        help="process whose exit stops this fixture: the test, or the listener")
    args = parser.parse_args()
    if args.lifetime_guard:
        lifetime_guard(args)
    guard = LifetimeGuard(args.test_owner if args.serve else args.owner,
                          Path(args.root) / "lifetime" if args.serve else None)
    if args.serve:
        # Run subprocess cleanup even when SSH disconnects or the finite watchdog fires.
        for signum in (signal.SIGHUP, signal.SIGTERM, signal.SIGALRM):
            signal.signal(signum, stop)
        signal.alarm(25)
        # sshd ends the session process when the client disconnects, but leaves this command
        # running. Stop once that parent or the fixture listener is gone.
        def watch(parent):
            while os.getppid() == parent and not stop_requested:
                time.sleep(0.05)
                if not alive(args.owner):
                    break
                time.sleep(0.45)
            os.kill(os.getpid(), signal.SIGTERM)
        threading.Thread(target=watch, args=(os.getppid(),), daemon=True).start()
        try:
            result = service(args)
        finally:
            begin_cleanup()
            guard.close()
        sys.exit(result)
    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, stop)
    try:
        main(args)
    finally:
        begin_cleanup()
        guard.close()
