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

## Pack Write Baseline

Run `cargo bench --bench pack_write` with the checked-in lockfile. Criterion uses 30 samples,
one-second warmup and five-second target measurement. Fixture creation and expected identity hashing
occur outside timing. Each measured call validates identities, sorts/deduplicates inputs, compresses
ordinary entries at zlib level 6, computes CRCs/checksums, and writes both artifacts to `io::sink`.
There is no filesystem I/O or retained output buffer; this measures artifact generation, not
installation or durable storage throughput.

Workloads contain 64 distinct 4 KiB blobs or four distinct 1 MiB blobs. Similar inputs repeat `x`
with distinct eight-byte prefixes. Pseudorandom inputs use a fixed-seed xorshift sequence per blob.
The harness prints exact input, pack and index sizes alongside Criterion throughput. Size ratios
compare pack bytes with payload bytes; index bytes are additional. These workloads illustrate
compressibility, not the savings available from delta selection across real revision histories.

Measured on 2026-09-23 with Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64, Apple M2 Max, using the
default optimized bench profile and flate2's lockfile-selected backend. The desktop session was not
CPU-isolated. The [CSV](benchmarks/pack-write-baseline.csv) retains median estimates and 95%
confidence intervals; the [source manifest](benchmarks/pack-write-baseline.sha256) identifies the
harness, library sources, manifest and lockfile. There is no performance acceptance threshold.

| Payloads (bytes) | Content      | Median (ms) | MiB/s | Pack bytes | Index bytes |
| ---------------- | ------------ | ----------- | ----- | ---------- | ----------- |
| 64 × 4096        | similar      | 1.107       | 225.9 | 2195       | 2864        |
| 64 × 4096        | pseudorandom | 3.774       | 66.2  | 263072     | 2864        |
| 4 × 1048576      | similar      | 8.030       | 498.1 | 4218       | 1184        |
| 4 × 1048576      | pseudorandom | 84.952      | 47.1  | 4195056    | 1184        |

Throughput uses uncompressed input bytes divided by the median time. Pack sizes include framing and
checksums. The final run followed validation jobs to reduce local contention; comparisons against
exploratory runs are not evidence of implementation regressions.

## Fetch Baseline

Run `cargo bench --bench fetch` with the checked-in lockfile. Criterion uses 30 samples, one-second
warmup and five-second target measurement windows. In the replay workloads below, fixture
construction and one real local upload-pack transfer per fixture occur outside timing. The measured
receive operation replays an in-memory v0 response through girt's advertisement parser, want
encoding, sideband decoding, pack validation, delta reconstruction, index generation, and
selected-tip connectivity. No server, network, disk publication, or ref update latency is included
in those replay measurements. The added local-process workloads are documented
[separately](#owned-transport-baseline).

The pack fixtures contain 16 or 256 similar, independently Git-generated blobs, plus a tree, commit
and annotated tag. Git's pack-objects chooses actual deltas. Respective uncompressed payload totals
are 574,362 and 9,185,238 bytes; pack lengths are 3,697 and 19,631 bytes. The advertised wanted tag
reaches every object. Replay responses add 107 bytes of framing/advertisement. A separate workload
parses 10,000 branch advertisements and selects nothing, isolating negotiation from pack processing.

Measured on 2026-09-23 with Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64, Apple M2 Max, optimized
bench profile and lockfile-selected flate2 backend. Inputs are already in memory and fixture
filesystem caches are warm; no CPU pinning or cold-cache measurements were performed. Desktop and
development activity may overlap sampling. The [CSV](benchmarks/fetch-baseline.csv) records median
estimates and 95% confidence intervals. Verify the retained source identity with
`shasum -a 256 -c docs/benchmarks/fetch-baseline.sha256`.

| Operation                                 | Median (ms) |
| ----------------------------------------- | ----------- |
| Protocol/import/connectivity, 19 objects  | 0.671       |
| Protocol/import/connectivity, 259 objects | 10.494      |
| Advertisement/select-none, 10,000 refs    | 3.198       |

These establish an initial baseline, with no numerical acceptance gate. Similar input deltas are
representative of repeated file revisions but not of all repositories. The measurements do not
establish deep-chain, multi-gigabyte, network, durable-publication, or concurrent-fetch throughput.
Resource-bound tests, rather than these throughput numbers, provide the memory/work-limit evidence.

## Push Baseline

Run `cargo bench --bench push` with the checked-in lockfile. Criterion uses 30 samples, one-second
warmup and five-second target measurement windows. Fixture construction, object-snapshot opening and
one real receive-pack transfer per fixture occur outside timing. Selection/read/validation/pack
construction starts from a packed `Objects` snapshot and ends with a buffered `PreparedPush`.
Prepared-protocol replay separately parses an in-memory advertisement, checks the expectation,
writes to a sink and parses a complete report. It excludes graph work and compression; sink writes
do not measure pack copying, disk or network throughput.

Fixtures contain 16 or 256 similar Git-generated blobs plus a tree, commit and annotated tag. The
selected tag reaches all 19 or 259 objects. Source packs use Git-selected deltas; outgoing packs use
girt's ordinary entries. This exercises packed reads, typed graph selection, payload validation,
hashing, sorting, compression and index generation into a sink. Filesystem caches are warm, pack
snapshots and protocol input are resident, and no cold-cache or CPU-pinned sampling is claimed.

Measured on 2026-09-23 with Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64, Apple M2 Max, optimized
bench profile and the lockfile-selected flate2 backend. Desktop/browser activity may overlap
sampling. The [CSV](benchmarks/push-baseline.csv) retains medians and 95% confidence intervals.
Verify the library, harness, fixture, manifest and lockfile with
`shasum -a 256 -c docs/benchmarks/push-baseline.sha256`.

| Objects | Payload bytes | Outgoing pack bytes | Prepare (ms) | Replay (µs) |
| ------- | ------------- | ------------------- | ------------ | ----------- |
| 19      | 574362        | 39189               | 4.276        | 0.581       |
| 259     | 9185238       | 622108              | 66.781       | 0.585       |

The source delta packs contain 3,697 and 19,631 bytes respectively. The larger outgoing packs are
expected from the writer's ordinary entries and full reachable transfer. These are initial baselines
with no numerical acceptance gate; they do not establish multi-gigabyte, deep-history,
large-advertisement, network or concurrent-push throughput. Protocol replay mostly measures
framing/status bookkeeping because sink writes discard pack bytes. Resource-bound tests provide
separate work and memory evidence.

## Owned Transport Baseline

Run `cargo bench --locked --bench fetch -- local-process-import-connectivity --noplot`. The same
19-/259-object fixtures as the fetch baseline above are created outside timing. Each iteration
starts a real local Git upload-pack, reads its advertisement, selects the tag, transfers and
validates the pack, and waits for exit/cleans up its process group. A fresh 30-second deadline
covers each iteration. No destination installation or reference update is measured. This measures
the owned transport path as a consumer uses it, including process startup and server work.

Measured on 2026-09-23 with Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64, Apple M2 Max, the
optimized bench profile, and the checked-in lockfile. Criterion uses 30 samples, a one-second warmup
and five-second target windows. Filesystem caches are warm; no CPU pinning or cold-cache
measurements were performed, and desktop activity may overlap sampling. The
[CSV](benchmarks/transport-baseline.csv) retains medians and 95% confidence intervals. Verify the
measured source identity with `shasum -a 256 -c docs/benchmarks/transport-baseline.sha256`.

| Operation                                      | Median (ms) |
| ---------------------------------------------- | ----------- |
| Local process/import/connectivity, 19 objects  | 23.170      |
| Local process/import/connectivity, 259 objects | 36.761      |

These are initial end-to-end baselines, not a comparison with the former blocking adapter. The exit
poll can add up to one 20-ms polling interval, subject to scheduling. No numerical performance gate
is imposed. Stall/cleanup tests establish interruption behavior; these throughput measurements do
not establish worst-case latency or Linux runtime behavior.

## Incremental Transfer Comparison

The `transfer-*` workloads in `benches/fetch.rs` and `benches/push.rs` use the existing
independently Git-generated 16/256-blob fixtures, then add one child commit sharing its parent's
tree. Sources are packed and cached; the new commit is loose. Fixture construction, local snapshot
opening and Git reference setup stay outside sampling. Run sequentially to avoid measurement
contention:

```sh
cargo bench --bench push -- transfer-
cargo bench --bench fetch -- transfer-
```

Push measures full selection/read/validation/ancestry/pack construction against the same operation
with the old commit supplied for exclusion. Both validate the entire selected history. Fetch
measures local server startup, protocol/import/combined-connectivity and cleanup with and without
preverified knowledge. Knowledge construction is measured separately and is not included in the
negotiated fetch time; installation/dependency rechecks and separate ref publication are excluded.
These are cached microbenchmarks, not network bandwidth or cold-disk predictions. Thirty Criterion
samples follow a one-second warmup and target five seconds of measurement. No numerical acceptance
gate is imposed.

Measurements use macOS arm64, Rust 1.98.1 and Git 2.55.0. The retained CSV and source fingerprints
identify the measured code and fixtures. Full and reduced pack bytes are wire pack contents,
excluding pkt-line framing, advertisements, acknowledgements and progress. Reduced push packs have
ordinary entries; fetch continues to accept self-contained Git-selected deltas.

Pack columns show objects / bytes; timing columns use milliseconds. Workload sizes count blobs.

| Workload    | Full pack     | Reduced pack | Full ms | Reduced ms | Prep ms  |
| ----------- | ------------- | ------------ | ------- | ---------- | -------- |
| Push (16)   | 19 / 39,222   | 1 / 193      | 4.407   | 1.839      | Included |
| Push (256)  | 259 / 622,141 | 1 / 194      | 67.522  | 27.570     | Included |
| Fetch (16)  | 19 / 3,646    | 1 / 193      | 24.247  | 23.591     | 1.740    |
| Fetch (256) | 259 / 19,584  | 1 / 194      | 37.630  | 24.689     | 26.937   |

Times above are Criterion's reported slope estimates where available and mean estimates otherwise;
[incremental-baseline.csv](benchmarks/incremental-baseline.csv) retains means and 95% confidence
intervals for every workload. [Source fingerprints](benchmarks/incremental-baseline.sha256) cover
source, harnesses, fixtures, manifest and lockfile. Push saves compression work while retaining full
selection validation. Fetch saves transfer/import work, but rebuilding knowledge for each local
fetch costs more than the full transfer at these sizes. Reused knowledge amortizes preparation; real
network effects are unmeasured. Dependency revalidation during installation adds local reads and is
not included in the table.

The disposable integration test also compares initial fetch (19 objects / 3,697 bytes), known-only
repeat (zero objects / zero pack bytes), and a child-commit fetch (one object / 182 bytes versus 20
objects / 3,763 bytes for the full branch/tag selection). Repeated conditional push sends an empty
32-byte pack and still receives server status. Different fixture messages/selections explain the
small byte differences from the timed workloads.

## Bounded Delta Comparison

The opt-in writer is compared with the unchanged ordinary encoding policy using
`cargo bench --bench pack_delta`. Criterion uses 20 samples, one second of warmup, and two seconds
of measurement per case (eight seconds for oversized inputs, allowing enough iterations). Both
policies include input identity validation, sorting/deduplication, zlib level 6, CRC/checksum
computation and index generation into `io::sink`; fixture construction is outside sampling. These
are in-memory measurements, not filesystem or transport timings.

The original workloads contain 16 objects each: fixed-seed binary data with separated one-byte
edits, binary data with insertions/deletions that shift offsets, generated Rust constant
declarations with individual changes, independently seeded random bytes, repeated bytes with unique
prefixes, 1 MiB + 1 byte random objects above the search-size limit, and eight-byte integers. This
broadens the prior repeated-fixture evidence without claiming representative coverage of all
repositories.

Measured on 2026-09-23 (local date), macOS arm64, Rust 1.98.1, Criterion 0.8, with the retained
Cargo.lock. The [CSV](benchmarks/delta-baseline.csv) preserves mean nanoseconds and 95% confidence
intervals, complete pack lengths, candidate attempts, search units and maximum chain depth.
[Source fingerprints](benchmarks/delta-baseline.sha256) identify the reproducible implementation,
options and fixtures. No Linux/Windows runtime result is implied.

| Workload       | Ordinary bytes | Delta bytes | Ordinary mean ms | Delta mean ms |
| -------------- | -------------- | ----------- | ---------------- | ------------- |
| Edited binary  | 1,048,992      | 66,743      | 16.815           | 19.801        |
| Shifted binary | 1,049,248      | 67,027      | 16.894           | 19.943        |
| Source         | 39,505         | 3,560       | 2.469            | 3.802         |
| Independent    | 1,048,992      | 1,048,992   | 16.816           | 87.837        |
| Repeated       | 1,508          | 843         | 2.150            | 5.964         |
| Oversized      | 16,780,144     | 16,780,144  | 334.451          | 335.169       |
| Tiny           | 224            | 224         | 0.138            | 0.141         |

Default delta search saves about 94% on the edited/shifted binary workloads for roughly 18% more CPU
time; generated source saves about 91% for 54% more time. Independent noise cannot benefit, but
costs about 5.2 times as much preparation time because bounded matching and unsuccessful zlib trials
still run. Repeated bytes save 665 bytes for roughly 2.8 times the time. Tiny and oversized cases do
not attempt delta search. These tradeoffs justify retaining ordinary compression as the default and
making limits explicit; they do not establish a universal tuning optimum.

Eligible 16-object cases attempt 54 candidates, consuming approximately 0.93 to 6.00 million search
units per pack; successful cases emit 15 deltas with maximum depth four. These counters exclude
identity hashing and zlib, which remain bounded by input and candidate size/count. Metadata is
linear in input count; the delta path adds at most a 32 KiB anchor table and roughly eight times the
configured eligible object size in live scratch, plus zlib/allocator overhead. With the default 1
MiB size bound this is roughly 8 MiB of additional scratch, compared with the ordinary streaming
compressor's fixed scratch. Payloads are borrowed. This is an analytical working-space bound, not an
RSS measurement or total-process memory promise. Increasing window/candidate/depth/size limits can
increase work and decode costs; matching the writer's depth/size policy to downstream read limits
remains the caller's responsibility.

The policy compares complete entry costs, so delta output cannot exceed ordinary output for the same
set. It deliberately uses REF_DELTA's 20-byte base identity rather than OFS_DELTA, preserving
compatibility with receive-pack servers that do not advertise `ofs-delta`. Benchmark values do not
include receiver reconstruction time, network latency, or the cost of selecting a push graph.

## Smart HTTP Loopback Baseline

`cargo bench --features http --bench http` runs Criterion against a persistent loopback Python
bridge invoking Git `http-backend` per request. Fixture generation, server startup, client/runtime
construction and local knowledge preparation occur outside sampling. Each sample includes service
discovery, any RPC, server process work, HTTP buffering and synchronous response validation; it
excludes installation. Filesystem caches are warm. These measurements describe this local stack, not
WAN latency, raw TLS throughput or a memory/RSS bound.

The independently generated workload contains 16 similar blobs plus a tree, commit and annotated tag
(574,362 payload bytes in the original 19 objects), then a new commit reusing the tree. Full fetch
selects the new commit, incremental fetch offers the previously verified history, and the known-only
case selects the existing tag. The incremental RPC transfers one new commit; it still pays
discovery, process startup and connectivity costs. No numerical acceptance threshold is set.

The 2026-09-23 run used macOS arm64, Rust 1.98.1, Git 2.55.0 and Python 3.14.7. Criterion used ten
samples, one-second warmup and a two-second target measurement period. Its 95% intervals were:

| Operation                       | Lower (ms) | Estimate (ms) | Upper (ms) |
| ------------------------------- | ---------: | ------------: | ---------: |
| Full download/validation        |     75.262 |        75.828 |     76.381 |
| Incremental download/validation |     73.671 |        74.219 |     74.763 |
| Known-only discovery/validation |     32.211 |        33.513 |     34.887 |

The full and incremental transfers differ by less than two milliseconds in this sample; server
startup, negotiation and local validation dominate the saved loopback bytes. Outliers and ten
samples do not support a general speedup/regression claim. TLS performance is not measured. Retained
CSV and source fingerprints in `docs/benchmarks/http-baseline.*` identify the measured
implementation and fixture. Fingerprints were recorded after formatting and minimum-version
corrections; resolved dependency versions and benchmark behavior did not change.

## SSH Loopback Baseline

Measured on 2026-09-23 on macOS arm64 with Rust 1.98.1, Git 2.55.0 and OpenSSH 10.3p1:

```sh
cargo bench --features ssh --bench ssh
```

The original 16-blob fixture has 19 objects and 574,362 payload bytes before the added incremental
commit. Timed operations include client process creation, a fresh SSH handshake/authentication,
forced-command dispatch to real Git upload-pack, pipe transfer, exit/group cleanup and synchronous
validation. Key generation, sshd startup, repository creation and known-history preparation are
outside timing. Storage is warm; SSH connection sharing is deliberately disabled. Criterion uses 10
samples, a one-second warmup and at least two seconds of measurement per case.

| Operation                  | Estimate (ms) | 95% interval (ms) |
| -------------------------- | ------------: | ----------------: |
| Full download + validation |        183.47 |     180.54–186.28 |
| Incremental + validation   |        184.66 |     183.57–185.65 |
| Known-only + validation    |        160.04 |     158.97–161.18 |

These small loopback transfers are dominated by connection/process/service overhead; they are not
WAN throughput or SSH algorithm comparisons. Incremental transfer saves objects without avoiding the
handshake. The fixture includes Python forced-command dispatch, and exit observation polls at 20 ms,
both of which contribute to elapsed time. Local runs put the known-only mean between 154 and 160 ms;
this is process/handshake timing evidence, not a claim of a reproducible code regression. No
numerical performance gate is imposed. Push graph and compression costs remain covered by the
existing push and delta benchmarks; this SSH benchmark measures the new waiting/transfer boundary.
RSS, cold-cache behavior and remote network latency are not measured.

[CSV estimates](benchmarks/ssh-baseline.csv) and
[source fingerprints](benchmarks/ssh-baseline.sha256) retain reproducible evidence. Linux runtime
performance is not established by this macOS run.

## Forward Delta Ordering

The original `tests/support/forward_delta.rs` fixture emits independent depth-64 REF_DELTA chains,
each in reverse dependency order, with unique eight-byte blob payloads. Literal-only deltas isolate
resolution ordering from compression matching. Setup is outside sampling. The measured operation
replays in-memory protocol bytes, imports and hashes all objects, builds an index, validates one
selected blob, and drops the result. It performs no filesystem or network I/O.

At review follow-up change `161fe6ed54b2` (the fixture and importer used for these measurements), a
chain of 65 entries requires 65 full resolver passes: 4,225 visits. The regression accepts that
exact budget and rejects one fewer; Git independently accepts the pack with `index-pack --strict`
and returns its tip with `cat-file`. The default ten-million-visit budget can therefore reject a
pack below the object-count limit when many deep chains have this ordering.

Measured on macOS arm64 (Darwin 25.6.0), rustc 1.98.1, Git 2.55.0, with the checked-in lockfile and
optimized bench profile. Criterion used ten samples, a one-second warmup and one-second measurement
target:

```sh
cargo bench --bench fetch -- forward-ref-delta --warm-up-time 1 --measurement-time 1 --sample-size 10
```

| Chains | Objects | Estimate  | 95% interval     |
| ------ | ------- | --------- | ---------------- |
| 1      | 65      | 288.59 µs | 287.12–289.63 µs |
| 16     | 1,040   | 4.8054 ms | 4.7054–4.8881 ms |
| 128    | 8,320   | 37.428 ms | 37.000–37.897 ms |

These are small-payload ordering measurements, not large-payload or peak-memory results. The
resolver and defaults remain unchanged: their existing work bound is explicit, and no required
consumer workload currently justifies a ready-queue redesign. A future need to accept larger
deep-forward packs should revisit the algorithm rather than silently raise the work allowance.

## Reference Enumeration and Deletion

The existing reference harness measures the new whole-store traversal and packed-file rewriting
paths because their work scales with the number of refs. Run:

```sh
cargo bench --bench references -- 'enumerate|delete-shadowed' --measurement-time 4
```

The original fixture has 10 or 10,000 packed tag refs alongside one loose branch. Enumeration reads
and validates packed records, traverses loose files and returns owned ordered values. Deletion
removes the last packed tag and a loose copy of the same tag, including locks, full packed parsing,
replacement-file publication, unlink and cleanup. Criterion `iter_batched` with `PerIteration`
resets both files outside the timed deletion. A separate enumeration workload has 1,000 loose tags
plus the loose branch. IDs are independently chosen nonzero bytes; no object lookup is measured.

These are warm-cache local filesystem measurements on macOS arm64, Rust 1.98.1 and Git 2.55.0,
collected on 2026-09-24 with 30 samples, one-second warmup and a four-second measurement target.
They measure neither cold storage nor concurrent writers, peak memory, fsync or crash durability. No
numerical regression gate or general performance improvement is claimed.

| Operation                      | Estimate (ms) | 95% interval (ms) |
| ------------------------------ | ------------: | ----------------: |
| Enumerate 10 packed tags       |        0.0752 |     0.0744–0.0761 |
| Delete from 10 packed tags     |        0.5341 |     0.5167–0.5641 |
| Enumerate 10,000 packed tags   |        3.8744 |     3.8582–3.8942 |
| Delete from 10,000 packed tags |        3.9309 |     3.8738–4.0002 |
| Enumerate 1,000 loose tags     |       26.1378 |   25.8339–26.5265 |

Retained [CSV estimates](benchmarks/reference-enumeration-baseline.csv) and
[source fingerprints](benchmarks/reference-enumeration-baseline.sha256) identify the measured
implementation and fixture. All workloads include the additional loose branch. Enumeration of many
loose files pays for individual filesystem reads; the packed workloads parse a single file. The
measurements establish a baseline, without a claim about other filesystems or platforms.

## Reference Transactions and Reflogs

The reference Criterion harness measures conditional publication with explicit log appends for 1 and
32 refs, and parses original in-memory reflogs with 10 and 10,000 records. Run:

```sh
cargo bench --bench references -- 'transactions/'
```

Publication includes packed/ref/log locking, precondition reads, ref replacement, append and
cleanup. Each iteration resets logs to empty outside timing with `iter_batched(PerIteration)`;
reference and log directories already exist and refs are warm after the first iteration. Parsing
includes full validation and owned record allocation. Transaction preparation likewise validates
each complete existing log, so append cost grows with existing log length; this baseline separates
parsing from filesystem publication instead of claiming constant-time updates.

These are warm local-storage/in-memory measurements on macOS arm64, Rust 1.98.1, using 30 samples,
one-second warmup and a two-second measurement target. They do not establish cold-storage latency,
peak memory, concurrent-writer performance or crash durability. No numerical performance gate is
imposed. Retained estimates and source fingerprints accompany the results below.

| Workload            | Estimate (ms) | 95% interval (ms) |
| ------------------- | ------------: | ----------------: |
| parse-log-10        |        0.0037 |     0.0037–0.0038 |
| parse-log-10000     |        3.7310 |     3.5886–3.9692 |
| publish-with-log-1  |        0.9251 |     0.8575–0.9784 |
| publish-with-log-32 |       17.8705 |   17.5210–18.2388 |

Retained [CSV estimates](benchmarks/reference-transaction-baseline.csv) and
[source fingerprints](benchmarks/reference-transaction-baseline.sha256) identify the measured
implementation. Samples include concurrent machine activity and are an initial baseline, not a claim
of isolated-system latency or improved performance.

## Refspec Mapping

`cargo bench --bench remotes` measures `Refspecs::map` over 10 and 10,000 generated full branch
names. Two positive wildcard specifications map each source to separate tracking/tag destinations,
and two negative patterns exclude names beginning with `1` or `3`. This returns 16 and 15,556 unique
mappings, respectively. Parsing, source construction and ID hashing are outside the timed operation.
Mapping includes input checks, pattern expansion/exclusion, destination validation,
duplicate/conflict checks, owned plan allocation and destruction. No filesystem or transfer
operation is measured.

The 2026-09-24 baseline uses Criterion 0.8.2, 30 samples, one-second warmup and two-second target
measurement per size, the default optimized Cargo bench profile, Rust 1.98.1, and macOS arm64 on
Apple M2 Max. Inputs are warm in memory; no CPU affinity, frequency or background-work controls were
applied. The command was `cargo bench --bench remotes > /tmp/girt-remotes-bench.log 2>&1`.

The [CSV](benchmarks/refspec-baseline.csv) retains Criterion median estimates and their 95%
confidence intervals in nanoseconds. The [source manifest](benchmarks/refspec-baseline.sha256)
identifies the measured harness and relevant sources; verify with
`shasum -a 256 -c docs/benchmarks/refspec-baseline.sha256`. Estimates describe this workload and
host, not a transfer latency, hard memory bound or cross-platform guarantee. No numerical gate is
set. The implementation scans sources for each positive spec and exclusions for each match; callers
must bound configuration/input sizes. Retained memory scales with source count and unique plan
entries.

## Fetch Orchestration Baseline

Run `cargo bench --bench fetch_workflow` with the checked-in lockfile. Criterion 0.8.2 uses 30
samples, a one-second warmup and a four-second measurement target. `plan/10` and `plan/10000` map
advertised full tag names into absent remote-tracking destinations against a prepared snapshot.
Setup is outside timing; mapping, decisions, allocation and result destruction are measured.

`install_publish_10` starts with a validated local transfer of one 4 KiB repeated-byte blob selected
by ten tag refs. Each iteration uses a new destination. Repository creation, destination preparation
and local transfer/validation are outside timing. The measured operation installs the pack/index,
checks HEAD layout, rereads selected-tip connectivity and publishes ten conditional remote-tracking
refs without reflogs. Temporary repository destruction is outside timing. No commit ancestry query
is needed in this creation workload; existing history benchmarks measure the underlying traversal.
This small workflow is not a large-history, WAN, HTTP or SSH throughput measurement.

Recorded on 2026-09-24 using rustc 1.98.1, Cargo 1.98.1, macOS 26.6.2 (25G83), Apple M2 Max and 96
GiB RAM. Storage uses the default local macOS temporary directory. Filesystem reads are warm: no
cache flush/eviction was attempted. The retained run followed the correctness checks with no
concurrent agent-started builds/tests. Desktop background activity, CPU placement and thermal state
were not controlled. The default optimized bench profile was used.

| Operation                          | Median µs | 95% CI, µs          |
| ---------------------------------- | --------- | ------------------- |
| Plan 10 refs                       | 6.131     | 6.078–6.174         |
| Plan 10,000 refs                   | 6217.531  | 6148.279–6382.637   |
| Install/verify/publish 10 new refs | 13725.028 | 13622.090–13939.181 |

The [CSV estimates](benchmarks/fetch-workflow-baseline.csv) retain median estimates and confidence
intervals in nanoseconds. The [source manifest](benchmarks/fetch-workflow-baseline.sha256)
fingerprints all library sources, the harness and Cargo files; verify with
`shasum -a 256 -c docs/benchmarks/fetch-workflow-baseline.sha256`. These are sampled operation
estimates, not latency percentiles or a performance target. Preliminary runs during implementation
are not comparison evidence. No numerical acceptance threshold or speedup/regression claim is made.

## Clone Planning Baseline

`cargo bench --bench clone` measures clone's advertisement-to-HEAD/refspec plan for 10 and 10,000
branches plus HEAD, with a valid symbolic HEAD capability. Generated names and arbitrary nonzero
SHA-1 IDs are original in-memory inputs; this operation does not read objects. Request preparation,
filesystem checks and advertisement generation are outside timing. Each iteration includes mapping,
HEAD selection, owned plan allocation and destruction. Existing fetch/transaction baselines cover
clone's reused installation and reference processing; no end-to-end clone latency claim is made.

The 2026-09-24 run used Criterion 0.8.2, 30 samples, one-second warmup and three-second target
measurement, with the checked-in lockfile and default optimized bench profile. The host was Apple M2
Max with 96 GiB RAM, macOS 26.6.2 (25G83), rustc 1.98.1 and Cargo 1.98.1. Input memory was warm;
there was no filesystem/network work inside timing. No concurrent agent-started builds/tests ran
during sampling. Desktop activity, CPU placement, frequency and thermal state were uncontrolled.

| Branches | Median µs | 95% CI, µs        |
| -------- | --------- | ----------------- |
| 10       | 7.277     | 7.227–7.337       |
| 10,000   | 6017.141  | 5997.925–6081.247 |

The [CSV estimates](benchmarks/clone-baseline.csv) retain medians and confidence intervals in
nanoseconds. The [source fingerprints](benchmarks/clone-baseline.sha256) cover library sources,
harness and Cargo files; verify with `shasum -a 256 -c docs/benchmarks/clone-baseline.sha256`.
Fingerprints include final contract-comment corrections after sampling; processing code is
unchanged. These are baseline operation estimates, not latency percentiles, a regression comparison
or a numerical acceptance threshold. Retained plans and temporary source lists scale with
advertisement size; transfer limits bound advertisements/wants, while arbitrary preview callers
bound their own inputs. Peak RSS and other platforms were not measured.

## Symbolic HEAD Detachment With Reflog

The reference harness now measures a stored symbolic-to-direct HEAD transaction with append logging.
Each iteration starts with an unborn branch, symbolic HEAD and an empty HEAD log. Resetting HEAD and
truncating the log happen outside timing with `iter_batched(PerIteration)`. The measured transaction
locks and rechecks HEAD and its absent branch dependency, publishes direct HEAD, appends its log and
releases locks. It performs no object lookup or clone transfer. Existing 1/32-direct-ref publication
cases run alongside it; their empty-log resets are also outside timing.

```sh
cargo bench --bench references -- 'transactions/(publish-with-log|detach-unborn)'
```

The 2026-09-24 run used Criterion 0.8.2, 30 samples, one-second warmup and two-second measurement
targets, with the checked-in lockfile and default optimized bench profile. Hardware was Apple M2
Max, 96 GiB RAM, on macOS 26.6.2 (25G83), rustc 1.98.1 and Cargo 1.98.1. Temporary filesystem
metadata was warm, without cache eviction. No agent-started builds or tests overlapped sampling;
background desktop activity and CPU placement/thermal state were uncontrolled.

| Operation                        | Median ms | 95% CI, ms      |
| -------------------------------- | --------- | --------------- |
| Detach unborn HEAD and log       | 0.8463    | 0.8407–0.8515   |
| Publish 1 direct ref with log    | 0.7206    | 0.7093–0.7321   |
| Publish 32 direct refs with logs | 17.3171   | 17.0730–17.7180 |

The [CSV estimates](benchmarks/reference-detach-baseline.csv) retain medians and 95% intervals in
nanoseconds. The [source manifest](benchmarks/reference-detach-baseline.sha256) fingerprints library
sources, the harness and Cargo files; verify with
`shasum -a 256 -c docs/benchmarks/reference-detach-baseline.sha256`. These are sampled warm-storage
operation baselines, not tail latency, peak-memory evidence or a numerical regression gate. The
historical transaction measurements were not collected as a controlled before/after comparison. No
Linux/Windows performance or end-to-end clone speed claim is made.

## Tree Comparison Baseline

`cargo bench --bench tree_compare` measures complete `Objects::compare_trees` calls, including warm
loose reads, identity verification, tree parsing and validation, name alignment, path construction,
output sorting and result destruction. Repository creation, fixture writes and reader opening are
outside timing. Each workload is checked once before sampling. The 10,000-leaf fixtures have 100
subdirectories of 100 files; repeated subtree IDs model shared content, but changed occurrences are
read per path without a decoded-object cache.

- Sparse: one subdirectory differs, yielding 100 changes and skipping 99 equal subtree pairs.
- Broad: all 100 subdirectories differ, yielding 10,000 changes.
- Deep: 512 nested directories end in one changed leaf; traversal reads both 513-tree chains.

The 2026-09-24 run used Criterion 0.8.2, 20 samples, one-second warmup and three-second target
measurement duration with the default optimized bench profile and checked-in lockfile. Host: Apple
M2 Max, 96 GiB RAM, macOS 26.6.2 (25G83), Rust/Cargo 1.98.1, aarch64-apple-darwin. Temporary files
used the local APFS volume; no cache eviction or synchronization was performed. No builds or tests
from this task overlapped sampling; desktop background load, CPU placement and thermals were
uncontrolled.

| Workload             | Median ms | 95% CI, ms      |
| -------------------- | --------- | --------------- |
| Sparse 100 of 10,000 | 0.1709    | 0.1676–0.1733   |
| Broad 10,000 changes | 8.6053    | 8.4597–8.8872   |
| Deep 512 directories | 23.3843   | 23.0378–23.7292 |

The [CSV estimates](benchmarks/tree-compare-baseline.csv) retain median estimates and 95% confidence
intervals in nanoseconds. The [source manifest](benchmarks/tree-compare-baseline.sha256) records
library, harness and Cargo source fingerprints; verify with
`shasum -a 256 -c docs/benchmarks/tree-compare-baseline.sha256`. These are warm-storage baselines,
not latency percentiles, peak RSS measurements or evidence for a numerical regression gate. The
sparse and broad cases exercise different amounts of traversal, not a controlled comparison with
another implementation. Deep results show the cost of many small verified reads, without claiming
cold-storage, packed-storage or cross-platform performance.

The retained run followed the final Rustdoc edits so its source manifest matches the checkout.
Criterion extended deep sampling to about 5.2 seconds to collect 20 samples. An earlier run of the
same executable comparison logic had medians of 0.198, 10.192 and 33.052 ms respectively; the
variation is not evidence of an implementation speedup. No controlled performance comparison was
attempted.

## Content Diff Baseline

`cargo bench --bench content_diff` measures complete pure `content_diff::diff` calls with auto
binary policy and default bounds, including scanning, line offsets, shortest-edit search, traceback
and result destruction. Fixtures are constructed before timing; there is no storage I/O. Workloads
are a one-line replacement in 1,000 unique lines, an insertion in 1,000 repeated/blank lines, 200
unrelated lines per side, and 100,000 unrelated lines per side rejected at the trace bound. Each
result class is checked before sampling. The rejection measurement reports the cost of refusing a
pathological comparison, not successful large-input diff throughput.

The 2026-09-24 run used Criterion 0.8.2, 20 samples, one-second warmup and three-second target
measurement duration, the default optimized bench profile and checked-in lockfile. Host: Apple M2
Max with 96 GiB RAM, macOS 26.6.2 (25G83), Rust/Cargo 1.98.1 and aarch64-apple-darwin. No builds or
tests from this task overlapped sampling; desktop load, CPU placement and thermals were
uncontrolled. Criterion extended the bounded-rejection collection to about 4.3 seconds.

| Workload                                       | Median µs | 95% CI, µs        |
| ---------------------------------------------- | --------- | ----------------- |
| One-line replacement / 1,000 lines             | 16.628    | 16.471–17.096     |
| Insertion / 1,000 repeated lines               | 11.246    | 11.144–11.432     |
| Unrelated / 200 lines per side                 | 381.544   | 379.866–384.860   |
| Trace-limit rejection / 100,000 lines per side | 6738.873  | 6711.566–6779.845 |

The [CSV](benchmarks/content-diff-baseline.csv) retains median estimates and 95% confidence
intervals in nanoseconds. The [source manifest](benchmarks/content-diff-baseline.sha256)
fingerprints library sources, harness, Cargo manifest and lockfile. Verify it from the repository
root with `shasum -a 256 -c docs/benchmarks/content-diff-baseline.sha256`. Criterion's original
estimates remain under `target/criterion/content_diff/`; fixture generation and all measured
operations are in `benches/content_diff.rs`.

These are operation baselines, not latency percentiles, peak-memory measurements, cross-platform
evidence or a controlled comparison with another implementation. The 200-line unrelated input
succeeds, while the 100,000-line case returns the documented trace-limit error. No arbitrary gate is
imposed. Input size alone does not predict cost: edit distance and repeated byte comparisons consume
separate work and trace budgets.

## Working-Tree Index Baseline

`cargo bench --bench index -- --sample-size 20 --warm-up-time 1 --measurement-time 3` measures pure
SHA-1 v2 parse and encode operations for 0, 100, 10,000 and 100,000 entries. Nonempty fixtures use
`directory/file-NNNNNNNN` paths, regular modes, one blob ID and zero stat data, with no extensions.
Construction and input encoding are outside sampling. Parse includes checksum verification, owned
path allocation, structural/stage/prefix validation and result destruction. Encode includes length
checks, allocation, canonical framing, checksum generation and result destruction. No storage I/O or
file locking is measured.

The 2026-09-24 run used Criterion 0.8.2, the default optimized bench profile, Rust/Cargo 1.98.1,
macOS 26.6.2 (25G83), aarch64-apple-darwin, and an Apple M2 Max with 96 GiB RAM. No builds/tests
from this task overlapped sampling. Background desktop activity, thermals and CPU placement were
uncontrolled. Criterion extended the largest workloads beyond the three-second target to collect 20
samples.

| Entries | Parse median µs | Encode median µs |
| ------- | --------------- | ---------------- |
| 0       | 0.0784          | 0.0958           |
| 100     | 18.792          | 10.377           |
| 10,000  | 2318.361        | 997.015          |
| 100,000 | 25141.811       | 10076.505        |

The [CSV estimates](benchmarks/index-baseline.csv) retain median estimates and 95% confidence
intervals in nanoseconds. The [source manifest](benchmarks/index-baseline.sha256) fingerprints
library sources, the harness, manifest and lockfile; verify with
`shasum -a 256 -c docs/benchmarks/index-baseline.sha256`. These are operation baselines without a
numerical acceptance gate, not tail latencies, peak-memory measurements, storage performance,
cross-platform evidence or a controlled comparison with another implementation. Path depth and
conflict shapes also affect validation cost; these fixtures represent shallow ordinary paths.

## Raw Working-Tree Status Baseline

`cargo bench --bench status -- --sample-size 20 --warm-up-time 1 --measurement-time 3` measures
complete `Repository::raw_status` calls against an explicit baseline tree matching the index. Each
fixture contains 100 or 1,000 root-level regular files with 1-KiB contents and one shared loose blob
identity. Clean cases have no changes. Changed cases replace every tenth working file with seven
bytes; their staged results remain empty. Untracked scanning is omitted. Fixture construction,
object/index publication and correctness assertions occur before sampling.

Timing includes opening the object reader, both index reads, verified tree traversal, repeated
per-entry blob verification, descriptor-relative name enumeration and working-file reads, result
construction and destruction. It excludes repository opening and HEAD resolution. Filesystem caches
are warm. The shared blob is deliberately verified for every index occurrence, matching the current
resource contract; these measurements do not represent a stat-cache optimization.

The 2026-09-24 run used Criterion 0.8.2, the default optimized bench profile, Rust/Cargo 1.98.1,
macOS 26.6.2 (25G83), aarch64-apple-darwin and an Apple M2 Max with 96 GiB RAM. No builds or tests
from this task overlapped sampling. Desktop load, thermals and CPU placement were uncontrolled.

| Working tree        | Files | Median ms | 95% CI, ms    |
| ------------------- | ----- | --------- | ------------- |
| Clean               | 100   | 4.479     | 4.236–4.902   |
| Ten percent changed | 100   | 4.329     | 4.194–4.374   |
| Clean               | 1,000 | 41.507    | 40.978–42.116 |
| Ten percent changed | 1,000 | 41.525    | 41.210–42.074 |

The [CSV](benchmarks/status-baseline.csv) retains medians and confidence intervals in nanoseconds.
The [source manifest](benchmarks/status-baseline.sha256) fingerprints library sources, the harness,
Cargo manifest and lockfile; verify with `shasum -a 256 -c docs/benchmarks/status-baseline.sha256`.
These are warm loose-storage baselines, not tail latencies, cold/packed-storage measurements,
peak-memory evidence or cross-platform performance claims. Similar clean/changed costs are expected
because both verify working content; small differences do not establish a performance improvement.
No numerical regression gate is imposed.

## Raw Tree Checkout Baseline

`cargo bench --bench checkout` measures complete `Repository::checkout_tree` calls for initial
checkout and replacement of every tracked file, with 10 or 100 root-level files and 1-KiB contents.
Each fixture has one shared loose blob per tree; updates use distinct old/new payloads. Repository,
objects and baseline worktree construction occur outside each timed iteration through Criterion's
`iter_batched_ref` with `PerIteration`. Temporary-repository cleanup is outside timing. The measured
operation includes preparation, locking, object verification, independent raw worktree checks,
namespace mutations, final verification, index publication and report destruction. Caches are warm.

The harness uses ten samples, one-second warmup and three-second measurement targets. Repeated name
enumeration protects mutation boundaries but can make wide-directory work quadratic. These
small-directory measurements do not establish large-checkout throughput or a stat-cache
optimization. No tail-latency, crash-durability, cold-storage, peak-memory or cross-platform claim
is made, and no numerical regression gate is imposed. Final estimates and source fingerprints are
retained below.

The 2026-09-24 run used Criterion 0.8.2, Rust/Cargo 1.98.1, the default optimized bench profile,
macOS 26.6.2 (25G83), aarch64-apple-darwin and an Apple M2 Max with 96 GiB RAM. No builds or tests
from this task overlapped the retained sampling run. Desktop activity, thermals and CPU placement
were uncontrolled.

| Checkout       | Files | Median ms | 95% CI, ms      |
| -------------- | ----- | --------- | --------------- |
| Initial        | 10    | 7.657     | 7.496–8.109     |
| Tracked update | 10    | 10.400    | 10.112–10.800   |
| Initial        | 100   | 80.406    | 78.811–82.126   |
| Tracked update | 100   | 114.926   | 113.744–116.986 |

The [CSV](benchmarks/checkout-baseline.csv) retains medians and confidence intervals in nanoseconds.
The [source manifest](benchmarks/checkout-baseline.sha256) fingerprints library sources, the
harness, Cargo manifest and lockfile; verify with
`shasum -a 256 -c docs/benchmarks/checkout-baseline.sha256`. These are operation baselines, not a
controlled performance comparison with another implementation.
