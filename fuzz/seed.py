#!/usr/bin/env python3
"""Generate fuzz seed corpora from repositories built by the installed Git executable.

Every seed is either bytes Git wrote or a small input written here from the format
specifications; no Git test or implementation source is used. Seeds are framed as the matching
`girt::fuzzing` entry point expects. Usage: `fuzz/seed.py [corpus-dir]` (default `fuzz/corpus`).
"""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

FORMATS = {"sha1": 0, "sha256": 1}
DIGEST = {"sha1": 20, "sha256": 32}
ENV = {
    **os.environ,
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_AUTHOR_NAME": "Fuzz Seed",
    "GIT_AUTHOR_EMAIL": "seed@example.com",
    "GIT_AUTHOR_DATE": "1700000000 +0130",
    "GIT_COMMITTER_NAME": "Fuzz Seed",
    "GIT_COMMITTER_EMAIL": "seed@example.com",
    "GIT_COMMITTER_DATE": "1700000000 -0700",
}


def git(repo, *args, data=None):
    return subprocess.run(
        ["git", "-C", str(repo), *args], input=data, env=ENV, check=True, capture_output=True
    ).stdout


def u32(value):
    return value.to_bytes(4, "big")


class Corpus:
    def __init__(self, root):
        self.root = root
        self.count = 0

    def add(self, target, name, data):
        directory = self.root / target
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_bytes(data)
        self.count += 1


def build_repository(path, format):
    """A small history with merges, a tag, nested trees, a symlink, an executable and deltas."""
    git(path.parent, "init", "-q", f"--object-format={format}", "-b", "main", path.name)
    work = Path(path)
    for round in range(4):
        (work / "dir" / "nested").mkdir(parents=True, exist_ok=True)
        text = "".join(f"line {i} of round {min(i, round)}\n" for i in range(200))
        (work / "file.txt").write_text(text)
        (work / "dir" / "nested" / f"n{round}").write_text(f"nested {round}\n")
        script = work / "run.sh"
        script.write_text("#!/bin/sh\necho run\n")
        script.chmod(0o755)
        link = work / "link"
        if not link.is_symlink():
            link.symlink_to("file.txt")
        git(work, "add", "-A")
        git(work, "commit", "-q", "-m", f"round {round}\n\nbody with trailer\n\nSigned-off-by: S <s@x>")
    git(work, "checkout", "-q", "-b", "side", "HEAD~2")
    (work / "side.txt").write_text("side\n")
    git(work, "add", "-A")
    git(work, "commit", "-q", "-m", "side")
    git(work, "checkout", "-q", "main")
    git(work, "merge", "-q", "--no-ff", "-m", "merge side", "side")
    git(work, "tag", "-a", "-m", "annotated\nmultiline", "v1")
    git(work, "tag", "-a", "-m", "tag of tag", "v1-outer", "v1")
    return work


def object_seeds(corpus, repo, format):
    selector = FORMATS[format]
    listing = git(repo, "cat-file", "--batch-all-objects", "--batch-check=%(objectname) %(objecttype)")
    kinds = {"commit": 0, "tree": 1, "tag": 2}
    for line in listing.decode().splitlines():
        oid, kind = line.split()
        loose = repo / ".git" / "objects" / oid[:2] / oid[2:]
        if loose.exists():
            corpus.add("loose", f"{format}-{oid}", bytes([selector]) + loose.read_bytes())
        if kind in kinds:
            payload = git(repo, "cat-file", kind, oid)
            corpus.add("object", f"{format}-{oid}", bytes([selector, kinds[kind]]) + payload)


def pack_seeds(corpus, repo, format):
    selector = FORMATS[format]
    git(repo, "repack", "-q", "-a", "-d", "-f", "--window=10")
    pack_dir = repo / ".git" / "objects" / "pack"
    for index in pack_dir.glob("*.idx"):
        pack = index.with_suffix(".pack").read_bytes()
        variants = {"v2": index.read_bytes()}
        v1 = Path(tempfile.mkdtemp()) / "v1.idx"
        git(repo, "index-pack", "--index-version=1", "-o", str(v1), str(index.with_suffix(".pack")))
        variants["v1"] = v1.read_bytes()
        shutil.rmtree(v1.parent)
        for version, idx in variants.items():
            end = len(pack) - DIGEST[format]
            corpus.add("pack_index", f"{format}-{version}", bytes([selector]) + u32(end) + idx)
            corpus.add("pack", f"{format}-{version}", bytes([selector]) + u32(len(idx)) + idx + pack)


def varint(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        out.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(out)


def delta_seeds(corpus):
    """Delta programs written from the pack format's copy/insert instruction encoding."""
    base = b"The quick brown fox jumps over the lazy dog.\n" * 4
    copy_all = bytes([0x80 | 0x10, len(base)])
    programs = {
        "insert": varint(len(base)) + varint(5) + b"\x05hello",
        "copy": varint(len(base)) + varint(len(base)) + copy_all,
        "mixed": varint(len(base)) + varint(13) + bytes([0x91, 4, 10]) + b"\x03abc",
        "offset": varint(len(base)) + varint(9) + bytes([0x93, 0x10, 0x00, 9]),
    }
    for name, program in programs.items():
        corpus.add("delta", name, u32(len(base)) + base + program)


def pkt(data):
    return f"{len(data) + 4:04x}".encode() + data


def service(repo, program, request=b"", version=None, advertise=False):
    env = dict(ENV, GIT_PROTOCOL=f"version={version}") if version else ENV
    args = ["git", program, "--stateless-rpc"] + (["--advertise-refs"] if advertise else []) + [str(repo)]
    return subprocess.run(args, input=request, env=env, check=True, capture_output=True).stdout


def first_ref(advertisement):
    """The first advertised (id, name) of a v0 advertisement."""
    length = int(advertisement[:4], 16)
    line = advertisement[4:length].split(b"\0")[0].rstrip(b"\n")
    oid, name = line.split(b" ", 1)
    return oid, name


def advertised_names(advertisement):
    names, rest = [], advertisement
    while rest[:4] not in (b"", b"0000"):
        length = int(rest[:4], 16)
        line = rest[4:length].split(b"\0")[0].rstrip(b"\n")
        names.append(line.split(b" ", 1)[1])
        rest = rest[length:]
    return names


def protocol_seeds(corpus, repo, format):
    """Upload-pack and receive-pack exchanges observed from the Git executable."""
    v0 = service(repo, "upload-pack", advertise=True)
    v2_caps = service(repo, "upload-pack", version=2, advertise=True)
    ls_refs = pkt(b"command=ls-refs\n") + b"0001" + pkt(b"peel\n") + pkt(b"symrefs\n") + b"0000"
    if format == "sha256":
        ls_refs = pkt(b"command=ls-refs\n") + pkt(b"object-format=sha256\n") + ls_refs[20:]
    v2_refs = service(repo, "upload-pack", ls_refs, version=2)
    corpus.add("fetch_discovery", f"{format}-v0", v0)
    corpus.add("fetch_discovery", f"{format}-v1", pkt(b"version 1\n") + v0)
    corpus.add("fetch_discovery", f"{format}-v2", v2_caps + v2_refs)
    oid, _ = first_ref(v0)
    request = pkt(b"want " + oid + b" side-band-64k ofs-delta\n") + b"0000" + pkt(b"done\n")
    response = service(repo, "upload-pack", request)
    corpus.add("fetch_receive", format, v0 + response)
    for name, stream in [("v0", v0), ("v2-caps", v2_caps), ("v2-refs", v2_refs), ("fetch", response)]:
        corpus.add("pkt_line", f"{format}-{name}", stream)

    selector = FORMATS[format]
    receive = service(repo, "receive-pack", advertise=True)
    names = sorted(advertised_names(receive))[:4]
    status = [pkt(b"unpack ok\n")]
    for position, name in enumerate(names):
        status.append(pkt((b"ok " if position % 2 == 0 else b"ng ") + name + (b"\n" if position % 2 == 0 else b" stale info\n")))
    status = b"".join(status) + b"0000"
    sideband = b"".join(pkt(b"\x01" + status[i:i + 1000]) for i in range(0, len(status), 1000)) + b"0000"
    for flags, report in [(0, status), (4, sideband)]:
        flags |= selector
        corpus.add("push", f"{format}-{flags}", bytes([flags]) + u32(len(receive)) + receive + report)


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "corpus")
    corpus = Corpus(root)
    with tempfile.TemporaryDirectory() as scratch:
        for format in FORMATS:
            repo = build_repository(Path(scratch) / format, format)
            object_seeds(corpus, repo, format)
            pack_seeds(corpus, repo, format)
            protocol_seeds(corpus, repo, format)
    delta_seeds(corpus)
    print(f"wrote {corpus.count} seeds under {root}")


if __name__ == "__main__":
    main()
