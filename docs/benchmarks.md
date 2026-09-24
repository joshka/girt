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

## Tag Baseline

Run `cargo bench --bench tags` with the checked-in lockfile and default optimized bench profile;
`just bench` includes the harness. Criterion uses 30 samples, one-second warmups, and two-second
measurement targets. Fixtures have a declared commit target, fixed tagger, name `v1`, one opaque
header, and a 128-byte or 65,536-byte message repeating
`Record snapshot: original benchmark content.` followed by LF. Setup is outside measurement.

Construction includes field cloning, validation, canonical encoding, and destruction. Parsing owns
both decoded fields and original bytes. Encoding copies the retained payload; identity hashes its
canonical tag header and bytes. Cached reads include file opening, decompression, validation,
parsing, and destruction after a warmup read. New writes hash, compress, create fanout directories,
and publish. Existing writes also read and compare stored bytes. Per-iteration batched store setup
and directory destruction are outside write measurements.

Measured on 2026-09-23 on Apple M2 Max, macOS 26.6.2 (25G83), arm64, with rustc 1.98.1 (`48a229cea`)
and Cargo 1.98.1 (`797e8a9bc`). Storage uses the default temporary directory on the internal APFS
SSD. Command: `cargo bench --bench tags > /tmp/girt-tag-benchmark.log 2>&1`. These are warm-cache
desktop measurements with uncontrolled background activity, no cache eviction, and no durability
synchronization. Repetitive messages compress well; these results do not characterize high-entropy
payloads or cold-storage latency. No numerical acceptance threshold is established.

The [CSV](benchmarks/tag-baseline.csv) retains Criterion median estimates and 95% confidence
intervals from `target/criterion/tags/{operation}/{128,65536}/new/estimates.json`. The
[source manifest](benchmarks/tag-baseline.sha256) records library, lockfile, and harness
fingerprints; verify with `shasum -a 256 -c docs/benchmarks/tag-baseline.sha256`. Timings do not
establish a total heap bound: parsed tags own both fields and retained payloads.

Payloads contain 269 and 65,677 bytes. Median estimates below are microseconds per operation, not
latency percentiles:

| Operation                  | 128-byte message | 65,536-byte message |
| -------------------------- | ---------------- | ------------------- |
| Construct with field clone | 1.212            | 7.596               |
| Parse                      | 0.667            | 4.019               |
| Encode (copy)              | 0.036            | 1.895               |
| Identity                   | 0.522            | 88.658              |
| Cached read                | 33.472           | 156.570             |
| New write                  | 640.830          | 871.978             |
| Existing write             | 702.841          | 1065.512            |

## Repository Opening Baseline

Measured on 2026-09-23 with Rust 1.98.1, Criterion 0.8.2, macOS arm64, and the default optimized
bench profile. Run:

```sh
cargo bench --bench repositories -- --sample-size 20 --warm-up-time 1 --measurement-time 2
```

The original harness constructs fixtures before timing. Parsing includes allocation and destruction
of the owned configuration. The small input contains repository version and bare settings; the large
input adds 1,000 remotes with URLs and fetch mappings. Opening repeatedly reads the same bare
repository, including metadata checks, configuration parsing and path canonicalization. This is a
warm filesystem-cache measurement, not a cold-storage estimate. No object payload is read.

| Operation                   | Criterion point estimate | 95% confidence interval |
| --------------------------- | ------------------------ | ----------------------- |
| Parse small config          | 300.20 ns                | 298.96–301.04 ns        |
| Parse 1,000 remote sections | 711.40 µs                | 693.21–726.96 µs        |
| Open bare repository, warm  | 40.556 µs                | 40.209–41.059 µs        |

The retained [estimates](benchmarks/repository-baseline.csv) and
[source fingerprints](benchmarks/repository-baseline.sha256) identify the measured implementation.
Check fingerprints with `shasum -a 256 -c docs/benchmarks/repository-baseline.sha256` from the
repository root. These are initial baselines without a numerical acceptance threshold or a claim
about other platforms, large real-world configurations, or cold storage.

## Reference Baseline

Run `cargo bench --bench references` with the checked-in lockfile. Criterion uses 30 samples,
one-second warmup and two-second measurement targets. Fixture setup and repository opening occur
outside timing; reads use warm filesystem caches. The harness measures a single loose direct read,
HEAD resolution through one symbolic hop, replacement of an existing loose ref with an exact old
value check, and lookups in packed files with 10 or 10,000 refs. Packed lookups include whole-file
validation and map allocation. Updates include acquiring/releasing packed and destination locks and
renaming the complete loose value, without reflogs or fsync. They do not measure durable writes.

The [CSV](benchmarks/reference-baseline.csv) retains median estimates and 95% confidence intervals
in nanoseconds. The [source fingerprint](benchmarks/reference-baseline.sha256) identifies the
measured sources, harness, manifest and lockfile; verify with
`shasum -a 256 -c docs/benchmarks/reference-baseline.sha256`. Recorded on 2026-09-23 with rustc
1.98.1, Cargo 1.98.1, macOS 26.6.2 (25G83), Apple M2 Max, and temporary directories on the internal
APFS SSD. The command was `cargo bench --bench references > /tmp/girt-references-bench.log 2>&1`.
There was no cache eviction, CPU pinning or control of desktop background activity. These are
initial warm-cache measurements, without a comparison to another library or a numerical acceptance
threshold.

| Operation                       | Median (µs) |
| ------------------------------- | ----------- |
| Loose direct read               | 20.8        |
| Resolve HEAD                    | 39.5        |
| Existing update without reflogs | 359.2       |
| Packed lookup, 10 refs          | 31.1        |
| Packed lookup, 10,000 refs      | 2947.5      |

The large packed workload exposes the cost of validating and allocating the entire file on every
lookup. These measurements establish a baseline for a future snapshot/index design; they do not
justify adding caching before its invalidation contract is defined.

## Pack Read Baseline

Run `cargo bench --bench packs` with the checked-in lockfile. Criterion uses 30 samples, one-second
warmup, and two-second measurement targets. Git fixture creation is outside timing. The shared
`tests/support/pack_git.rs` workload builds 16 or 256 similar 35 KiB blobs plus one tree, commit,
and annotated tag. Git produces actual OFS_DELTA entries; `verify-pack` and entry headers confirm
the encoding. The harness records the selected delta's base/depth and pack/index sizes.

Validated opening reads pack/index bytes and checks SHA-1 trailers, index structure, and entry CRCs.
Indexed misses measure the live loose-path miss and binary search. Successful reads also include
zlib decoding and identity verification; delta reads include base decoding, reconstruction, and both
identities. Pack snapshots are already in memory for reads and reconstructed objects are not cached.
Filesystem caches are warm, including repeated nonexistent loose-path lookups.

The [CSV](benchmarks/pack-baseline.csv) retains median estimates and 95% confidence intervals in
nanoseconds. The [source fingerprints](benchmarks/pack-baseline.sha256) identify the measured
sources, fixture builder, harness, manifest, and lockfile. Verify with
`shasum -a 256 -c docs/benchmarks/pack-baseline.sha256`. Measurements use rustc 1.98.1, Git 2.55.0,
macOS 26.6.2 (25G83), and Apple M2 Max, with temporary files on the internal APFS SSD. The command
was `cargo bench --bench packs > /tmp/girt-packs-bench.txt 2>&1`, run on 2026-09-23. There was no
cache eviction, CPU pinning, or control of desktop background activity. These are initial baselines,
without a comparison to another library or a numerical acceptance threshold.

| Operation                     | 16 blobs, median (µs) | 256 blobs, median (µs) |
| ----------------------------- | --------------------- | ---------------------- |
| Open and validate, warm files | 52.95                 | 85.61                  |
| Indexed absence               | 1.28                  | 1.30                   |
| Indexed ordinary blob         | 62.09                 | 61.35                  |
| Indexed delta reconstruction  | 100.96                | 105.71                 |

The selected ordinary payloads are 35,841 and 35,842 bytes. Both selected deltas have depth one;
their programs are 28 and 27 bytes. The corresponding pack/index sizes are 3,697/1,604 and
19,631/8,324 bytes. The difference between input object volume and pack size reflects the
deliberately similar fixture content. This baseline does not establish deep-chain, multi-gigabyte
pack, random cold-storage, or highly concurrent performance. Other development checks ran during
this sample; small differences between runs should not be treated as implementation regressions.

## Commit History Baseline

Run `cargo bench --bench history` with the checked-in lockfile. Criterion uses 20 samples,
one-second warmup, and four-second measurement targets. Each workload contains 256 commits: a linear
chain or 85 successive diamonds (two branches and a merge per diamond). Fixtures are original
Git-written commits with alternating timestamps, repacked before timing. Both traversal and
merge-base queries include storage reads, identity checks, commit parsing, graph construction, and
cycle validation. Pack snapshots are already in memory; decoded objects are not cached. Merge-base
endpoints are commits 254 and 255; traversal starts at commit 255.

The source fingerprint and CSV in `docs/benchmarks/history-baseline.*` retain reproducible source
identity and median estimates with 95% confidence intervals. Measurements use rustc 1.98.1, Git
2.55.0, macOS arm64, and warm filesystem caches, without cache eviction or CPU pinning. Other
development checks may run concurrently. These initial measurements establish no numerical
acceptance threshold and do not establish cold-storage or large-repository performance.

Recorded on 2026-09-23 with `cargo bench --bench history`. Verify sources with
`shasum -a 256 -c docs/benchmarks/history-baseline.sha256`.

| Workload                           | Median (ms) |
| ---------------------------------- | ----------- |
| linear/merge-bases-packed-256      | 2.936       |
| linear/walk-packed-256             | 2.955       |
| merge-heavy/merge-bases-packed-256 | 2.699       |
| merge-heavy/walk-packed-256        | 2.698       |

These values describe the final diamond workload. Criterion comparisons against earlier exploratory
fixture shapes are not implementation performance comparisons.
