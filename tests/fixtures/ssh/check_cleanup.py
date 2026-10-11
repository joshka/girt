#!/usr/bin/env python3
"""Interrupt actual nextest runs; require fixture listeners and services to disappear.

Run after compiling the SSH tests, under the shared timing lock when other runs are active.
This is an original process-lifetime regression probe, not an SSH implementation fixture.
"""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time


def processes():
    listing = subprocess.check_output(["ps", "-axo", "pid=,ppid=,command="], text=True)
    return {int(pid): (int(parent), command)
            for pid, parent, command in (line.strip().split(None, 2)
                                         for line in listing.splitlines())}


def descendants(root, listing):
    owned = {root}
    while True:
        expanded = owned | {pid for pid, (parent, _) in listing.items() if parent in owned}
        if expanded == owned:
            return owned
        owned = expanded


def wait_for_fixture(runner, repository):
    end = time.monotonic() + 20
    while time.monotonic() < end and runner.poll() is None:
        listing = processes()
        owned = descendants(runner.pid, listing)
        servers = [pid for pid in owned if str(repository / "tests/fixtures/ssh/server.py")
                   in listing[pid][1] and "--serve" not in listing[pid][1]]
        services = [pid for pid in owned if "--serve" in listing[pid][1]
                    and "girt-ssh-" in listing[pid][1]]
        if servers and services:
            return listing[servers[0]][0]
        time.sleep(.01)
    raise AssertionError("SSH fault service did not start before interruption")


def assert_clean():
    end = time.monotonic() + 10
    while True:
        remaining = {pid: command for pid, (_, command) in processes().items()
                     if "girt-ssh-" in command
                     and Path(command.split()[0]).name.startswith(("sshd", "python"))}
        if not remaining:
            return
        assert time.monotonic() < end, f"fixture processes survived: {remaining}"
        time.sleep(.02)


def interrupt(repository, mode):
    command = ["cargo", "nextest", "run", "--all-features", "--test", "ssh",
               "-E", "test(stalled_service_has_a_deadline)"]
    with tempfile.TemporaryDirectory(prefix="girt-cleanup-probe-") as directory:
        if mode == "timeout":
            config = Path(directory) / "nextest.toml"
            config.write_text('[profile.default]\nslow-timeout = { period = "1s", terminate-after = 1 }\n')
            command += ["--config-file", str(config)]
        with (Path(directory) / "nextest.log").open("w+") as log:
            runner = subprocess.Popen(command, cwd=repository, stdout=log, stderr=log,
                                      start_new_session=True)
            try:
                test = wait_for_fixture(runner, repository)
                if mode == "kill-test":
                    # SIGKILL runs no Rust destructors. Python must notice its owner's death.
                    signal_pid, signum = test, signal.SIGKILL
                else:
                    signal_pid, signum = runner.pid, signal.SIGINT
                if mode != "timeout":
                    os.kill(signal_pid, signum)
                    if mode == "double-interrupt":
                        time.sleep(.02)
                        if runner.poll() is None:
                            os.kill(runner.pid, signal.SIGINT)
                result = runner.wait(timeout=30)
                assert result != 0, "interrupted run unexpectedly succeeded"
                assert_clean()
            except BaseException:
                runner.kill()
                runner.wait()
                log.seek(0)
                print(log.read())
                raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repeat", type=int, default=3)
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[3]
    assert_clean()
    for repetition in range(args.repeat):
        for mode in ("interrupt", "double-interrupt", "kill-test", "timeout"):
            interrupt(repository, mode)
            print(f"PASS {mode} {repetition + 1}/{args.repeat}: no girt-ssh- processes", flush=True)
