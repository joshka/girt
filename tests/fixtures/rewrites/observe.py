"""Original executable-only Git/jj rename observations; no upstream fixtures or source."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ENV = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "JJ_"))}


def observe():
    observations = []
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        ENV.update(JJ_CONFIG=str(root / "absent"), GIT_CONFIG_GLOBAL=str(root / "absent"),
                   GIT_CONFIG_NOSYSTEM="1", GIT_AUTHOR_NAME="Original",
                   GIT_AUTHOR_EMAIL="original@example.com", GIT_COMMITTER_NAME="Original",
                   GIT_COMMITTER_EMAIL="original@example.com")

        def run(*args, data=None):
            p = subprocess.run(args, cwd=root, env=ENV, input=data, capture_output=True, timeout=60)
            assert p.returncode == 0, (args, p.stderr)
            return p.stdout

        run("git", "init", "-q")
        run("jj", "git", "init", "--colocate")

        def tree(entries):
            rows = []
            for name, content in entries.items():
                oid = run("git", "hash-object", "-w", "--stdin", data=content).strip()
                rows.append(b"100644 blob " + oid + b"\t" + name.encode() + b"\n")
            return run("git", "mktree", data=b"".join(rows)).strip().decode()

        def probe(name, before, after):
            old = run("git", "commit-tree", tree(before), data=b"original old").strip().decode()
            new = run("git", "commit-tree", tree(after), "-p", old, data=b"original new").strip().decode()
            run("git", "update-ref", "refs/heads/probe", new)
            run("jj", "git", "import")
            g = run("git", "diff-tree", "-r", "--no-commit-id", "--name-status", "-M50%", "-C50%", "--no-rename-empty", "-l1000", old, new).decode()
            j = run("jj", "--ignore-working-copy", "diff", "--from", old, "--to", new, "--summary").decode()
            observations.append(dict(name=name, git=g, jj=j, old=old, new=new))

        cases = [
            ("half", b"a\nb\nc\nd\n", b"a\nb\nx\ny\n"),
            ("reorder", b"a\nb\nc\nd\n", b"d\nc\nb\na\n"),
            ("unterminated", b"a\nb", b"a\nc"),
            ("long", b"a" * 128, b"a" * 64 + b"b" * 64),
            ("binary", b"\0" + b"a" * 127, b"\0" + b"a" * 63 + b"b" * 64),
            ("binary_lines", b"\0\na\nb\nc\n", b"\0\na\nb\nx\n"),
            ("crlf", b"a\r\nb\r\nc\r\n", b"a\nb\nc\n"),
            ("grow_half", b"a\nb\n", b"a\nb\nc\nd\n"),
            ("grow_below", b"a\nb\n", b"a\nb\nc\nd\nx\n"),
            ("shrink_half", b"a\nb\nc\nd\n", b"a\nb\n"),
            ("unequal_lines", b"a" * 100 + b"\nb\nc\n", b"a" * 100 + b"\nx\ny\n"),
            ("empty", b"", b""),
        ]
        for name, before, after in cases:
            probe(name, {"old": before}, {"new": after})
        probe("modified_copy", {"old": b"a\nb\nc\nd\n"}, {"old": b"changed\n", "new": b"a\nb\nc\nx\n"})
        probe("unchanged_copy", {"old": b"same\n"}, {"old": b"same\n", "new": b"same\n"})
        probe("rename_and_copy", {"old": b"same\n"}, {"new": b"same\n", "other": b"same\n"})
        probe("tie", {"a": b"same\n", "b": b"same\n"}, {"new": b"same\n"})
        # Attributes/config are supplied as data; no filter command may run.
        run("git", "config", "filter.probe.clean", "a-command-that-must-not-exist")
        run("git", "config", "filter.probe.required", "true")
        run("git", "config", "diff.probe.textconv", "a-command-that-must-not-exist")
        attrs = b"* text eol=lf filter=probe diff=probe\n"
        probe("attributes", {"old": b"a\nb\nc\nd\n", ".gitattributes": attrs}, {"new": b"a\nb\nx\ny\n", ".gitattributes": attrs})
        print(json.dumps(dict(git=run("git", "--version").decode().strip(), jj=run("jj", "--version").decode().strip(), observations=observations), indent=2))


if __name__ == "__main__":
    observe()
