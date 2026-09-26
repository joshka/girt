#!/usr/bin/env python3
"""Synthetic approved Git credential helper for local HTTP outcome tests."""
import pathlib
import sys

log = pathlib.Path(sys.argv[1])
action = sys.argv[2]
sys.stdin.buffer.read()
with log.open("a", encoding="ascii") as output:
    output.write(action + "\n")
if action == "get":
    sys.stdout.buffer.write(b"username=u\npassword=p\n\n")
