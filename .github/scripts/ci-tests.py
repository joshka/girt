"""Build the existing CI selections once, or run a native archive partition."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SUITES = [
    {"name": "All-feature tests", "args": "--all-features"},
    {
        "name": "Core-only runtime without tracing",
        "args": "--no-default-features --lib --test layout_shallow --test worktree_create --test "
        "worktree_admin --test object_ids --test sha256 --test commits --test decoding --test "
        "packs --test index --test colocation --test references --test references_portable "
        "--test reflog_expiry --test retention --test retention_noop --test "
        "retention_integrity --test retention_survivors --test retention_repack_contracts",
    },
    {
        "name": "Tracing without network dependencies",
        "args": "--no-default-features --features tracing --test tracing",
    },
    {
        "name": "Tracing HTTP lifecycle",
        "args": "--no-default-features --features tracing,http --test tracing",
    },
    {
        "name": "HTTP-only runtime without tracing",
        "args": "--no-default-features --features http --test http --test http_portable",
    },
    {
        "name": "SSH-only runtime without tracing",
        "args": "--no-default-features --features ssh --test ssh",
    },
]


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    mode = sys.argv[1]
    directory = Path("ci-archives")
    evidence = Path("ci-results")
    directory.mkdir(exist_ok=True)
    evidence.mkdir(exist_ok=True)
    manifest_path = directory / "manifest.json"
    if mode == "build":
        archives = []
        for index, suite in enumerate(SUITES):
            archive = directory / f"{index}.tar.zst"
            print(f"::group::Build {suite['name']}", flush=True)
            subprocess.run(
                [
                    "cargo",
                    "+stable",
                    "nextest",
                    "archive",
                    "--locked",
                    "--timings",
                    *suite["args"].split(),
                    "--archive-file",
                    str(archive),
                ],
                check=True,
            )
            archives.append(
                {
                    "file": archive.name,
                    "name": suite["name"],
                    "sha256": checksum(archive),
                }
            )
            print("::endgroup::", flush=True)
        manifest_path.write_text(
            json.dumps(
                {
                    "sha": os.environ["GITHUB_SHA"],
                    "os": os.environ["RUNNER_OS"],
                    "archives": archives,
                },
                indent=2,
            )
        )
    elif mode == "run":
        manifest = json.loads(manifest_path.read_text())
        if (
            manifest["sha"] != os.environ["GITHUB_SHA"]
            or manifest["os"] != os.environ["RUNNER_OS"]
        ):
            raise RuntimeError("Archive source revision or native OS does not match")
        failed = False
        for archive in manifest["archives"]:
            path = directory / archive["file"]
            if checksum(path) != archive["sha256"]:
                raise RuntimeError(f"Archive checksum mismatch: {path}")
            print(f"::group::Run {archive['name']}", flush=True)
            # Ubuntu's /tmp is quota-limited tmpfs; archives belong on the runner disk.
            # Reuse this extraction for listing and execution, then remove it per archive.
            with tempfile.TemporaryDirectory(
                dir=os.environ["RUNNER_TEMP"]
            ) as extracted:
                # Fixtures embed CARGO_MANIFEST_DIR: build and shard checkouts must
                # share their absolute path, as native GitHub-hosted runners do.
                common = [
                    "--workspace-remap",
                    str(Path.cwd()),
                    "--partition",
                    os.environ["PARTITION"],
                    "--profile",
                    "ci",
                ]
                with (evidence / f"{path.stem}.json").open("w") as listing:
                    subprocess.run(
                        [
                            "cargo-nextest",
                            "nextest",
                            "list",
                            "--archive-file",
                            str(path),
                            "--extract-to",
                            extracted,
                            *common,
                            "--message-format",
                            "json",
                        ],
                        stdout=listing,
                        check=True,
                    )
                inventory = json.loads((evidence / f"{path.stem}.json").read_text())
                if inventory["test-count"] == 0:
                    raise RuntimeError(f"Archive contains no tests: {path}")
                target = Path(extracted) / "target"
                common.extend(
                    [
                        "--cargo-metadata",
                        str(target / "nextest/cargo-metadata.json"),
                        "--binaries-metadata",
                        str(target / "nextest/binaries-metadata.json"),
                        "--target-dir-remap",
                        str(target),
                    ]
                )
                # A valid nonempty archive can assign zero tests to one hash partition.
                result = subprocess.run(
                    ["cargo-nextest", "nextest", "run", *common, "--no-tests=warn"],
                    check=False,
                )
            failed |= result.returncode != 0
            print("::endgroup::", flush=True)
        return int(failed)
    else:
        raise ValueError(f"Unknown mode: {mode}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
