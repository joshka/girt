# Pending bookmark export in jj

The `git export` / `100-pending-bookmarks` row exports 100 jj-only bookmarks from a non-colocated
Git store. [Issue 216](https://github.com/joshka/girt/issues/216) tracks its remaining fixed cost.
The previous measurement on girt 0.17.0 and adapter `5a6b4b03` was 113 ms against stock jj's 87 ms
on medium, and 136 ms against 110 ms on Linux.

## Reference transactions

The adapter called `update_git_ref` once per bookmark. Each new bookmark reached `set_git_ref`,
which submitted a one-element `References::transaction`.

A separate filesystem-call probe on fresh medium and Linux copies counted 100 packed-ref reads and
100 packed-ref lock cycles for girt. Stock made 101 packed-ref reads and 100 packed-ref lock cycles.
Neither trace contained a packed-ref rewrite or an `fsync` call. Girt also created 100 reflog locks
and 100 staged loose-ref temporary files that did not appear in the stock trace. These counts cover
the exported refs; jj's operation-store writes are separate.

The patched medium probe reduces packed-ref reads and packed-ref lock cycles to one. It still
creates one ref lock, one staged ref file and one reflog lock per exported ref. Batching removes
repeated transaction setup while preserving per-ref publication and logging.

The lever from the [performance checklist](https://github.com/joshka/girt/issues/599) is
**batched ref updates**. The measurement does not identify per-ref fsync as the cause.

## Adapter change

The [adapter change](https://github.com/joshka/jj/commit/0f44d2fd658678eaa3acc5dafb3a90717e9561af)
collects new bookmark creations in one `References::transaction`. Every edit retains
`Expected::Absent` and the existing reflog policy. Moves, deletions and tags keep the existing
per-ref path.

If the batch fails, export retries through the existing per-ref path. That path recognizes refs
already at the desired target, including a prefix published before an I/O failure, and reports
conflicting refs individually. A conflicting ref sends the batch through this fallback, so the
benchmark improvement applies to successful batches of new bookmarks. Publication remains observable
one ref at a time; this change adds no crash-atomicity promise.

## Results

Fresh copies use the same export setup and command as
`scripts/jj-e2e/bench-ops.py --matrix --ops export`. Each tool is warmed before measurement.
Before/after and after/stock each run in ABBA order twice, with four samples per tool per
comparison. Small after/stock has a predefined additional ABBA×2 repeat, with all eight samples per
tool retained. Copies are made before acquiring the shared `shlock` timing lock and deleted after
releasing it; each acquisition gates on a one-minute load below 24.

`/usr/bin/time -l` invokes jj directly. The matrix harness normally wraps the command in a shell;
direct invocation makes its retired-instruction counter describe jj. The
[retained samples](benchmarks/jj-export-216.json) include user CPU, instructions, load and both
order comparisons. The comparisons use separate ABBA blocks.

Before/after paired medians:

| Fixture | Before (ms) | After (ms) |
| ------- | ----------: | ---------: |
| small   |      122.51 |     116.33 |
| medium  |      174.52 |     152.59 |
| linux   |      197.84 |     211.23 |

After/stock paired medians and separate order ratios:

| Fixture | After (ms) | Stock (ms) | After / stock | First order | Reverse order |
| ------- | ---------: | ---------: | ------------: | ----------: | ------------: |
| small   |     100.88 |      78.32 |        1.288× |      1.246× |        1.279× |
| medium  |     159.47 |     168.05 |        0.949× |      1.090× |        1.081× |
| linux   |     232.17 |     298.85 |        0.777× |      0.824× |        0.709× |

Full `git fsck --full` passes on every fixture after patched export. Every measured sample exports
exactly 100 intended refs. Linux fsck exits zero with nine historical-tag warnings for missing
tagger entries; export does not rewrite those objects.

The medium after/stock order ratios are 1.090× and 1.081×; Linux is 0.824× and 0.709×. Small remains
above target at 1.288× overall (1.246× and 1.279× by order). The adapter batching change therefore
meets the requested medium/Linux comparisons, with a remaining small-fixture gap.

The timing spreads are wide. The paired Linux before/after median changes from 197.84 ms to 211.23
ms, so this run does not establish a Linux wall-time speedup. Retired instructions fall from 1,059
million to 945 million. Medium instructions fall from 826 million to 732 million, and small from 669
million to 572 million. These counts show reduced work without attributing the entire earlier fixed
gap to transaction setup.

## Rejected mode-change experiment

A follow-up probe found 100 `fchmod` calls on the 100 staged loose refs. An experiment created each
temporary with the captured lock permissions, checked the resulting mode and corrected mismatches.
It retained ownership checks and the write/sync/rename sequence, and passed 563 reference tests
including four permission cases. The benchmark applied only that patch to girt 0.17.0, avoiding
unrelated changes from current main.

It removed all 100 `fchmod` calls, but did not improve latency in both before/after orders. The
experiment is not included. Its patch and all samples remain in the retained evidence. Girt's
library code, public API and dependency requirements are unchanged by this PR.

## Validation

The jj Git and backend tests pass, including new cases for 100 successful creations with one reflog
entry each, and independently created matching and conflicting refs. The latter checks that export
preserves the conflicting Git ref and leaves its jj tracking target absent.
