# Blob Performance Baseline

## Reproduce

- Run `just bench`, or `cargo bench --bench blobs`. Criterion writes reports and statistical
  estimates under `target/criterion/`. A benchmark name filter can select a workload, for example
  `cargo bench --bench blobs -- write_new`.
- Use the checked-in lockfile and default optimized Cargo bench profile. Criterion 0.8.2 handles
  sampling and analysis. No native CPU flags are required.
- `cargo test --all-targets` runs Criterion's short test mode for the blob benchmarks. `just check`
  compiles the harness through Clippy without timing it. There are no CI performance gates.
- Run without competing workloads where possible. Compare repeated runs on the same machine and
  filesystem before attributing a change to code.

## Workloads and Measurements

- Inputs are 64 bytes, 4 KiB, 64 KiB, and 1 MiB. Repeated content is the byte `x`;
  low-compressibility content uses a fixed-seed xorshift64 sequence, not a cryptographic random
  source.
- Each benchmark uses 30 samples, a one-second warmup, and a two-second target measurement duration.
  Criterion chooses iteration counts and reports estimates with confidence intervals. Setup and
  analysis can make elapsed runtime longer than the measurement duration.
- **Hash:** `iter` measures `ObjectId::for_blob`, including header construction and in-memory
  hashing.
- **Cached read:** setup writes and reads a blob before `iter` repeatedly calls `read_blob` on that
  object. Opening, decompression, validation, hashing, and returned-buffer destruction are measured.
- **New write:** `iter_batched_ref` creates an empty object directory before each iteration. The
  timed `write_blob` includes hashing, fanout creation, compression, temporary-file creation,
  publication, and temporary-file removal.
- **Existing write:** `iter_batched_ref` pre-populates a fresh object directory before each
  iteration. The timed write includes compression into a temporary file, attempted publication,
  reading and comparing the existing object, and temporary-file removal.
- Batched writes use `BatchSize::PerIteration` to limit live temporary filesystem state. Fixture
  construction and directory destruction are outside the timer. Fixture bytes are also generated
  outside measurements. Criterion and `black_box` prevent unused-result elimination.

## Recorded Environment

- Date: 2026-09-23.
- Measured revision: `3f46f0e0b07135e14a735e4aba49d9e2ea27751b`. Later source changes are not
  represented by these timings.
- Source identity: [SHA-256 manifest](benchmarks/blob-baseline.sha256) records the measured harness,
  library sources, Cargo manifest, and lockfile. Verify from the measured revision's repository root
  with `shasum -a 256 -c docs/benchmarks/blob-baseline.sha256`.
- Host: Apple M2 Max, 96 GiB RAM, `aarch64-apple-darwin`.
- OS: macOS 26.6.2, build 25G83.
- Toolchain: rustc 1.98.1 (`48a229cea`, LLVM 22.1.8); Cargo 1.98.1 (`797e8a9bc`).
- Storage: internal SSD, APFS with FileVault; default macOS temporary directory on the Data volume.
  The volume was approximately 97% full. No disk-cache eviction or filesystem synchronization was
  performed.
- Command: `cargo bench --bench blobs > /tmp/girt-criterion-baseline.log 2>&1`.
- The harness ran serially in one process on a normal desktop session. CPU placement, clock rate,
  thermal state, and background activity were not controlled.

## Results

The [CSV summary](benchmarks/blob-baseline.csv) records Criterion's median estimates and their 95%
confidence intervals in nanoseconds per operation. These are estimates from sampled iterations, not
individual-operation latency percentiles. The table gives median estimates in microseconds.

| Bytes   | Content      | Hash (µs) | Cached read (µs) | New write (µs) | Existing write (µs) |
| ------- | ------------ | --------- | ---------------- | -------------- | ------------------- |
| 64      | repeated     | 0.2       | 21.8             | 444.6          | 436.9               |
| 64      | pseudorandom | 0.2       | 18.7             | 444.6          | 439.6               |
| 4096    | repeated     | 4.2       | 25.8             | 449.7          | 446.7               |
| 4096    | pseudorandom | 4.2       | 23.3             | 491.4          | 490.9               |
| 65536   | repeated     | 65.0      | 98.0             | 564.7          | 641.4               |
| 65536   | pseudorandom | 64.9      | 99.5             | 1447.6         | 1579.6              |
| 1048576 | repeated     | 1041.5    | 1283.7           | 2486.0         | 3694.1              |
| 1048576 | pseudorandom | 1037.6    | 1339.0           | 20700.0        | 22946.5             |

To regenerate the CSV after a complete run, execute this from the repository root:

```sh
python3 - <<'PYTHON'
import csv
import json
from pathlib import Path

with open("docs/benchmarks/blob-baseline.csv", "w") as output:
    writer = csv.writer(output)
    writer.writerow(["operation", "content", "bytes", "median_ns", "ci95_low_ns", "ci95_high_ns"])
    for size in (64, 4096, 65536, 1048576):
        for content in ("repeated", "pseudorandom"):
            for operation in ("hash", "read_cached", "write_new", "write_existing"):
                path = Path("target/criterion/blobs") / f"{operation}_{content}" / str(size)
                estimate = json.loads((path / "new/estimates.json").read_text())["median"]
                interval = estimate["confidence_interval"]
                assert interval["confidence_level"] == 0.95
                writer.writerow([operation, content, size, estimate["point_estimate"],
                                 interval["lower_bound"], interval["upper_bound"]])
PYTHON
```

## Interpretation and Limits

- This is an initial Criterion baseline, not a performance target or comparison with another
  library. It replaces the manual-harness results; differences between harnesses are not library
  regressions.
- Reads are warm-cache measurements. Recently created files and directories also favor write paths.
  These numbers do not establish cold-storage throughput or durable-write latency.
- Existing writes compress a temporary object before checking the existing file. The benchmark
  measures this validation path rather than a cheap existence check.
- Timer overhead, allocation, filesystem state, fixed workload order, and scheduling affect results.
  Bootstrap confidence intervals do not capture all environmental variation. Repeat measurements
  before drawing conclusions about small changes.
- Measurements do not establish memory bounds, collision resistance, power-loss durability, or
  performance on other platforms. Functional tests and documented contracts remain separate
  evidence.

## In-Memory Tree Baseline

Run `cargo bench --bench trees` with the checked-in lockfile and default optimized bench profile.
`just bench` also runs blob and loose-tree harnesses. The tree harness uses Criterion with 30
samples, a one-second warmup, and a two-second target measurement duration per workload. Fixtures
are constructed outside the measured operation: 16 or 1,024 entries, cycling through the five
supported modes, with names `entry-000000` onward and fixed raw object IDs.

Parsing measures `Tree::parse` including owned entry/name allocation and destruction. Encoding
measures `Tree::encode` including returned-buffer allocation and destruction. Construction, sorting,
validation, and identity hashing are not timed. All input is already in memory; no filesystem
operations or cache-eviction claims apply. These are initial baselines without numerical performance
gates, not comparisons with another implementation.

Measured on 2026-09-23 on Apple M2 Max, macOS 26.6.2 (25G83), arm64, with rustc 1.98.1 (`48a229cea`)
and Cargo 1.98.1 (`797e8a9bc`). Command:
`cargo bench --bench trees > /tmp/girt-trees-benchmark.log 2>&1`. This normal desktop session had
uncontrolled background activity; repeat on the same machine before interpreting small changes.

The [source manifest](benchmarks/tree-baseline.sha256) identifies the measured library and harness
at revision `189a130956647216a6b37a0292b0f5410c5b53a8`, before later documentation edits. From that
revision, verify with `shasum -a 256 -c docs/benchmarks/tree-baseline.sha256`. The
[CSV](benchmarks/tree-baseline.csv) retains Criterion median estimates and 95% confidence intervals
from `target/criterion/trees/{parse,encode}/{16,1024}/new/estimates.json`. Medians below are sampled
operation-time estimates, not individual-operation latency percentiles.

| Entries | Payload bytes | Parse median (µs) | Encode median (µs) |
| ------- | ------------- | ----------------- | ------------------ |
| 16      | 637           | 0.696             | 0.500              |
| 1,024   | 40,755        | 34.807            | 11.059             |

## Loose-Tree Storage Baseline

Run `cargo bench --bench loose_trees` with the checked-in lockfile and default optimized bench
profile. `just bench` includes this harness. Criterion uses 30 samples, a one-second warmup, and a
two-second target measurement per workload. Fixtures contain 16 or 1,024 regular-file entries with
names `entry-000000` onward and IDs derived from distinct `contents-{index}` blob payloads. Their
encoded sizes are 640 and 40,960 bytes. Fixture construction is outside the timer.

Cached reads measure file opening, decompression, framing and identity verification, owned tree
parsing, and returned-tree destruction after one warmup read. New writes include tree encoding,
hashing, compression, fanout creation, and publication. Existing writes additionally read and
compare the stored payload. Writes use `iter_batched_ref` with `BatchSize::PerIteration`; empty or
populated store setup and directory destruction occur outside the measurement.

Measured on 2026-09-23 with rustc 1.98.1 (`48a229cea`) and Cargo 1.98.1 (`797e8a9bc`) on Apple M2
Max, macOS 26.6.2 (25G83), arm64. Storage uses the default temporary directory on the internal APFS
SSD. Command: `cargo bench --bench loose_trees > /tmp/girt-loose-trees-benchmark.log 2>&1`. No cache
eviction or synchronization is performed; these are warm-cache desktop measurements with
uncontrolled background activity, not cold-storage or durable-write latency.

The [source manifest](benchmarks/loose-tree-baseline.sha256) identifies the measured source and
harness. Verify with `shasum -a 256 -c docs/benchmarks/loose-tree-baseline.sha256`. The
[CSV](benchmarks/loose-tree-baseline.csv) retains Criterion median estimates and 95% confidence
intervals from `target/criterion/loose_trees/{operation}/{16,1024}/new/estimates.json`. No numerical
performance threshold or cross-platform claim is established.

Median estimates in microseconds per operation (not individual-operation latency percentiles):

| Entries | Cached read (µs) | New write (µs) | Existing write (µs) |
| ------- | ---------------- | -------------- | ------------------- |
| 16      | 25.0             | 430.8          | 458.8               |
| 1024    | 175.1            | 1205.5         | 1320.1              |

## Commit Baseline

Run `cargo bench --bench commits` with the checked-in lockfile and default optimized bench profile;
`just bench` includes this harness. Criterion uses 30 samples, one-second warmups, and two-second
measurement targets. Each fixture has two parents, fixed identities/dates, an opaque multiline
header, and a 128-byte or 65,536-byte message repeating the ASCII line
`Record snapshot: original benchmark content.` with a final LF in the repeated pattern. Fixture
construction occurs outside measurement.

Construction measures cloning the fixture fields, validation, canonical encoding, and destruction.
Parsing includes owned decoded fields, retention of the original payload, and destruction. Encoding
measures copying the retained payload, not rebuilding fields. Identity hashes the canonical commit
header and retained bytes. Cached reads include file opening, decompression, framing and identity
checks, parsing, and destruction after a warmup read. New writes include hashing, compression,
fanout creation, and publication. Existing writes additionally read and compare stored payloads.
Per-iteration batched storage setup and directory destruction occur outside write measurements.

Measured on 2026-09-23 on Apple M2 Max, macOS 26.6.2 (25G83), arm64, with rustc 1.98.1 (`48a229cea`)
and Cargo 1.98.1 (`797e8a9bc`). Storage uses the default temporary directory on the internal APFS
SSD. Command: `cargo bench --bench commits > /tmp/girt-commits-benchmark.log 2>&1`. These are
warm-cache desktop measurements with uncontrolled background activity; no cache eviction or
synchronization establishes cold-storage or durable-write latency. No numerical acceptance gate or
comparison with another implementation is implied.

The [CSV](benchmarks/commit-baseline.csv) retains Criterion median estimates and 95% confidence
intervals from `target/criterion/commits/{operation}/{128,65536}/new/estimates.json`. The
[source manifest](benchmarks/commit-baseline.sha256) identifies the measured library, lockfile, and
harness; verify it with `shasum -a 256 -c docs/benchmarks/commit-baseline.sha256`. Small and large
payloads contain 432 and 65,840 bytes. Construction and reads retain both decoded content and raw
payload bytes; timings do not establish a total heap bound. Messages are repetitive and compress
well, so storage results do not characterize high-entropy payloads.

Median estimates below are microseconds per operation, not latency percentiles:

| Operation                  | 128-byte message | 65,536-byte message |
| -------------------------- | ---------------- | ------------------- |
| Construct with field clone | 2.067            | 6.969               |
| Parse                      | 0.907            | 3.366               |
| Encode (copy)              | 0.032            | 1.318               |
| Identity                   | 0.598            | 64.495              |
| Cached read                | 23.938           | 110.505             |
| New write                  | 442.445          | 572.093             |
| Existing write             | 435.018          | 660.978             |
