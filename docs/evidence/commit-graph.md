# Commit-Graph History Timing

Measured on 2026-10-18 with Git 2.56.0, an Apple M2 Max and macOS 26.6.2. The release harness uses
merged implementation revision `38b9524966e3` plus the profiling-only changes in this PR. The
original Git history has 82,500 reachable commits. The selected range contains 351 commits:

- Tip: `f57c9a73884400380bddd172acc88b569834f909`.
- Base: `8043d64be4ab216efcb6de2d61c668f8142ed4d4` (`tip~100`).

The same Git-written, verified standalone graph is present for every variant. A reads commit
objects; B retains the default indexed reader; G is Git. Both girt modes include default graph
loading during opening, then A removes the index before querying. This isolates the query
implementation while retaining the opening cost in process measurements.

Five warm G/A/B/B/A/G blocks per operation ran under `/tmp/girt-timing.lock` with competing builds
and benchmarks paused. One-minute load was 10.375 at entry and 11.005 at exit; individual recorded
slots ranged from 10.104 to 11.005. Git oracle commands disable system/global configuration. Each
sample records its command, wall time, peak RSS, phase timings and load in the
[raw JSON lines](commit-graph/timings.jsonl). The metadata records binary and graph SHA-256 hashes.

| Operation | Variant            | Median process time | Peak RSS   |
| --------- | ------------------ | ------------------- | ---------- |
| Range     | Git log            | 16.24 ms            | 21.55 MiB  |
| Range     | Git rev-list count | 9.88 ms             | 13.56 MiB  |
| Range     | Object-backed girt | 913.41 ms           | 139.80 MiB |
| Range     | Indexed girt       | 110.63 ms           | 38.83 MiB  |
| Ancestry  | Git merge-base     | 10.48 ms            | 10.98 MiB  |
| Ancestry  | Object-backed girt | 906.63 ms           | 139.80 MiB |
| Ancestry  | Indexed girt       | 114.48 ms           | 38.77 MiB  |

Git range timing uses `git log --oneline --no-decorate A..B`, which renders commit summaries; girt
collects identities and prints a count. `git rev-list --count A..B` provides a second control with
less rendering. Ancestry compares `git merge-base --is-ancestor A B` with girt's boolean query. All
counts and ancestry answers match. These are warm process measurements, not cold-cache results.

The indexed query phase itself takes a median 1.369 ms for the range and 0.075 ms for ancestry.
Indexed opening takes 101.763 ms and 106.401 ms respectively. Object-backed query phases take
779.811 ms and 776.625 ms, with opening around 104 ms. Phase medians need not sum to process
medians. The graph removes most query work, but opening dominates short indexed queries: process
totals are still about 6.8 times Git log and 10.9 times Git ancestry. These results do not establish
parity.

## Reproduce

Build the harness before obtaining a quiet timing window:

```sh
CARGO_BUILD_JOBS=3 cargo build --release --example commit_graph_profile
```

Prepare a bare copy of the selected Git history, write and verify its graph with isolated Git
configuration, then run the prepared driver when one-minute load is below 12 and the timing lock is
free. The driver acquires the lock without waiting and releases it in `finally`:

```sh
python3 benches/commit_graph.py /path/to/git.git \
  target/release/examples/commit_graph_profile \
  f57c9a73884400380bddd172acc88b569834f909 \
  8043d64be4ab216efcb6de2d61c668f8142ed4d4 --rounds 5 > timings.jsonl
```

Fixture setup used a shared bare clone referencing an existing local object store; no packs were
copied during the measured window. Both girt modes and Git see the same packs and graph. Automatic
graph writing during GC/maintenance remains separate from explicit index APIs.
