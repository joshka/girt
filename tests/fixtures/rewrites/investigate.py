"""Bounded independent Git/draft scoring and pairing observations, seed 180011.

No upstream implementation/test source or fixtures. Each case records complete original bytes,
raw operation results and IDs. Mismatches are reported, never normalized into equality.
"""
import argparse
import json
import os
from pathlib import Path
import random
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("adapter", type=Path)
    parser.add_argument("--format", choices=["sha1", "sha256"], default="sha1")
    args = parser.parse_args()
    adapter = args.adapter.resolve()
    rng = random.Random(180011)
    observations = []
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        env.update(GIT_CONFIG_GLOBAL=str(root / "absent"), GIT_CONFIG_NOSYSTEM="1")

        def run(*argv, data=None):
            p = subprocess.run(argv, cwd=root, env=env, input=data, capture_output=True, timeout=20)
            assert p.returncode == 0, (argv, p.stderr)
            return p.stdout

        def git(*argv, data=None):
            return run("git", *argv, data=data)

        git("init", "--bare", "--template=", "--object-format=" + args.format, ".")
        cache = {}

        def tree(entries):
            directories = {}
            rows = []
            for path, data in sorted(entries.items()):
                if "/" in path:
                    parent, name = path.split("/", 1)
                    directories.setdefault(parent, {})[name] = data
                    continue
                if data not in cache:
                    cache[data] = git("hash-object", "-w", "--stdin", data=data).strip()
                rows.append(b"100644 blob " + cache[data] + b"\t" + path.encode() + b"\0")
            for name, children in directories.items():
                rows.append(b"040000 tree " + tree(children).encode() + b"\t" + name.encode() + b"\0")
            return git("mktree", "-z", data=b"".join(rows)).strip().decode()

        def probe(name, old_files, new_files, copy=False):
            old, new = tree(old_files), tree(new_files)
            flags = ["-C50%"] if copy else []
            actual = git("diff-tree", "-r", "--no-commit-id", "--name-status", "-z", "-M50%", "--no-rename-empty", "--diff-filter=RC", *flags, old, new)
            draft = None if copy else run(str(adapter), str(root), old, new)
            observations.append(dict(name=name, copies=copy, before={k: v.hex() for k, v in old_files.items()}, after={k: v.hex() for k, v in new_files.items()}, old=old, new=new, git=actual.decode(), draft=None if draft is None else draft.decode(), equal=None if draft is None else actual == draft))

        def pair(name, old, new):
            probe(name, {"old": old}, {"new": new})

        # Uniform values distinguish fixed framing from unexplained equivalence classes.
        for size in [1, 16, 31, 32, 33, 63, 64, 65, 96, 127, 128, 129]:
            for suffix, end in [("raw", b""), ("lf", b"\n")]:
                pair(f"uniform/{size}/{suffix}", b"a" * size + end, b"b" * size + end)
                pair(f"half/{size}/{suffix}", b"a" * size + end, b"a" * (size // 2) + b"b" * (size - size // 2) + end)
        for value in [0, 1, 32, 65, 66, 97, 98, 127, 128, 255]:
            pair(f"values/{value}", b"a" * 64, bytes([value]) * 64)
        for period in [2, 3, 4, 7, 8, 16, 31, 32]:
            old = (b"abcdefghijklmnopqrstuvwxyz012345"[:period] * 128)[:128]
            new = old[period:] + old[:period]
            pair(f"period/{period}/rotation", old, new)
            pair(f"period/{period}/changed", old, old[:64] + bytes(v ^ 16 for v in old[64:]))
        for index in range(40):
            old = bytes(rng.randrange(33, 127) for _ in range(128))
            position = rng.randrange(128)
            new = old[:position] + bytes([old[position] ^ 16]) + old[position + 1:]
            pair(f"random-span/{index}/{position}", old, new)
        for index in range(20):
            old = bytes(rng.randrange(256) for _ in range(256))
            new = old[:128] + bytes(rng.randrange(256) for _ in range(128))
            pair(f"random-binary/{index}", old, new)
        # Source-like regular content with independently numbered equal-width lines.
        for retained in [49, 50, 51, 74, 75, 76, 89, 90, 99]:
            old = b"".join(f"let original_{i:03} = value_{i:03};\n".encode() for i in range(100))
            new = b"".join(f"let {'original' if i < retained else 'replaced'}_{i:03} = value_{i:03};\n".encode() for i in range(100))
            pair(f"ordinary/{retained}", old, new)
        for size in [63, 64, 65, 127, 128]:
            pair(f"separator/{size}", b"header\n" + b"-" * size + b"\nfooter\n", b"header\n" + b"=" * size + b"\nfooter\n")
        # Basename preselection can take the weaker match before global scoring.
        lines = [f"original line {i:03}\n".encode() for i in range(100)]
        original = b"".join(lines)
        stronger = b"".join(lines[:99]) + b"replaced line 099\n"
        for retained in [50, 60, 70, 74, 75, 76, 80, 89, 90, 95]:
            same_name = b"".join(lines[:retained]) + b"".join(f"replaced line {i:03}\n".encode() for i in range(retained, 100))
            for copies in [False, True]:
                probe(f"basename/{retained}/copies-{copies}", {"a/file": original}, {"b/file": same_name, "c/other": stronger}, copies)
        result = dict(seed=180011, git=git("--version").decode().strip(), format=args.format,
                      cases=len(observations), mismatches=sum(o["equal"] is False for o in observations), observations=observations)
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
