# Blob Performance Baseline

## Reproduce

- Run `just bench`, or `cargo bench --bench blobs`. Criterion writes reports and statistical
  estimates under `target/criterion/`. A benchmark name filter can select a workload, for example
  `cargo bench --bench blobs -- write_new`.
- Use the checked-in lockfile and default optimized Cargo bench profile. Criterion 0.8.2 handles
  sampling and analysis. No native CPU flags are required.
- `cargo test --all-targets` runs Criterion's short test mode for all 32 benchmarks. `just check`
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
- Source identity: [SHA-256 manifest](benchmarks/blob-baseline.sha256) records the measured harness,
  library sources, Cargo manifest, and lockfile. Verify from the repository root with
  `shasum -a 256 -c docs/benchmarks/blob-baseline.sha256`.
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
