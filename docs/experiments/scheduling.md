# Bounded consumer scheduling experiment

Whole-operation workers kept the current-thread executor responsive for this object/history
workload. The original experiment identified borrowed fetch history as an obstacle to caller-owned
dispatch. The subsequent ownership revision replaces that prototype API: HTTP/SSH downloads now own
optional `Arc<KnownHistory>` history and can move directly to blocking workers. Scheduling remains
consumer-owned; local storage stays synchronous. Timing results below describe the original run, not
a new benchmark of network fetch or installation.

## Reproduction and scope

Run from the repository root:

```sh
cargo run --release --all-features --example scheduling > docs/experiments/scheduling.csv
python3 examples/scheduling/check_handoff.py
```

The example requires Git. The compile probes use cached dependencies with `--offline`, seed their
lockfile from this checkout, and compile temporary crates without executing network operations. The
current probes require both owned-history and no-history handoffs to compile. The historical
rejections in `handoff.txt` belong to the original API and are not expected from current code. The
scheduling example enables Tokio's existing `sync` feature through a development dependency. Tokio
is already a project dependency, licensed MIT; no new library runtime dependency is introduced.

Acceptance evidence:

- Compare inline and admitted blocking operations on a current-thread runtime at limits 1 and 4.
- Record timer lateness, operation and request wall time, batch completion, and peak work counts.
- Prove queued cancellation prevents dispatch and started cancellation does not stop this operation.
- Observe completion and permit recovery even when the requesting receiver is dropped.
- Original run: compile actual HTTP/SSH borrowed handoffs and consumer workarounds.
- Ownership revision: compile and execute direct owned handoffs using the current public APIs.
- Use disposable fixtures and join all work before cleanup.

This is a bounded scheduling trace, requested to test responsiveness and lifecycle behavior. It is
not a statistical processing benchmark: Criterion baselines remain the mechanism for sustained
throughput/regression claims. No such claim or performance threshold follows from these samples.

## Method and environment

Recorded on 2026-09-23, Apple M2 Max, 96 GiB RAM, macOS 26.6.2 (25G83), Rust 1.98.1 (`48a229cea`),
Tokio 1.53.1, Git 2.55.0, release optimization. The library parent is SSH-adapter revision
`4a0c5815cf1c0a6076f5908e215665f1b1598d5d`. Source fingerprints are in
[scheduling.sha256](scheduling.sha256), including the original probe source and manifest at
experiment revision `a35f3005`; those fingerprints are historical and do not describe subsequently
edited files. Raw observations are in [scheduling.csv](scheduling.csv).

Each of eight scenario fixtures is generated independently through Git fast-import: a linear
512-commit history with one tree and one deterministic, poorly compressible 2 MiB blob. Git unpacks
it into loose objects; packed scenarios repack and prune loose copies. The original generated data
uses no implementation-source fixtures. Git commands run sequentially in temporary bare repositories
with inherited `GIT_*` variables removed and global/system config disabled. The experiment makes no
changes to real repositories or persistent Git configuration.

One operation opens the repository and object snapshot, reads and verifies the blob, and walks all
512 commits, asserting byte length and commit count. These are whole synchronous calls. Packed
opening includes pack/index reading and verification; this is not a pure in-memory lookup test. Each
batch submits exactly eight operations sharing fixture paths. Limits 1 and 4 control admitted
workers, while Tokio's blocking-thread limit is 4. Each operation owns its object snapshot. Inline
execution is intrinsically serial on the current-thread runtime even when admission permits four.

A 1 ms timer starts before the workload. Each sample measures lateness relative to its next
requested wake, then schedules the next tick from the actual wake. Missed ticks are skipped rather
than invented as catch-up observations. Timer resolution, OS scheduling and the stop wake affect the
numbers. An inline batch produced only one sample: its reported p95 is that single observation, not
a meaningful latency distribution. Worker batches produced 13–55 observations.

`operation_p50_ms` is the upper middle of eight actual synchronous operation durations, excluding
admission and worker dispatch. `request_p50_ms` additionally includes queuing and response delivery.
`batch_ms` measures submission through joined completion. Counts distinguish work waiting for
admission from actually executing closures; admitted closures could briefly await a Tokio thread.
The measured running peaks were 1 and 4. Peak waiting was 8 in every case because all requests were
submitted before admission ran. The fixed eight-request batch is the queue bound; this helper does
not enforce admission for an unbounded service. No saturation rejection policy is being proposed.

“First-touch” labels the first batch against a freshly generated fixture, without prior girt reads.
Only the earliest operation(s) encounter it first; later operations in that batch can reuse cache.
“Warm” immediately repeats eight operations on that fixture. Fixture creation itself populates OS
caches. Neither label establishes cold disk behavior. The runtime and blocking threads are reused;
ordering is fixed, with loose before packed and inline before workers.

## Observations

Warm batch results, rounded to milliseconds:

| Storage | Execution | Limit | Batch | Operation p50 | Request p50 | Maximum timer lateness |
| ------- | --------- | ----- | ----- | ------------- | ----------- | ---------------------- |
| Loose   | Inline    | 1     | 131.4 | 16.4          | 81.2        | 130.4                  |
| Loose   | Inline    | 4     | 132.6 | 16.8          | 82.1        | 131.6                  |
| Loose   | Workers   | 1     | 129.8 | 16.6          | 81.2        | 1.9                    |
| Loose   | Workers   | 4     | 61.5  | 31.8          | 61.2        | 1.8                    |
| Packed  | Inline    | 1     | 90.0  | 11.3          | 55.4        | 89.0                   |
| Packed  | Inline    | 4     | 91.1  | 11.5          | 58.0        | 90.1                   |
| Packed  | Workers   | 1     | 91.5  | 11.5          | 57.4        | 1.6                    |
| Packed  | Workers   | 4     | 30.9  | 14.9          | 29.0        | 1.5                    |

First-touch batches showed 87.9–142.3 ms maximum lateness inline and 1.5–2.4 ms with workers. Across
both cache labels, inline batches stalled the timer for 87.9–142.3 ms; worker maxima were 1.5–2.4
ms. Four workers reduced batch time here but increased individual operation duration, especially on
loose storage. This supports bounded offloading for responsiveness; it does not establish four as
the right service-wide concurrency limit.

## Cancellation, ownership and cleanup

The executable asserts four deterministic lifecycle cases:

1. Hold the only permit, queue a request, set its cancellation flag, then release admission. The
   supervisor reports `cancelled-before-dispatch`; no blocking operation runs. Cancellation is
   observed at admission, so a cancelled request retains its queue entry until admission advances.
1. Drop a queued requesting receiver without setting cancellation. The supervisor still admits and
   joins the operation. Dropping interest does not imply cancelling accepted work.
1. Pause a started operation after repository/snapshot opening, set cancellation, then release it.
   The blob read and history walk still finish. These girt operations have no cancellation argument.
1. At that same started boundary, set cancellation and drop the requesting receiver. The independent
   supervisor still observes `completed`, and the permit remains held until the worker returns.

The start/release handshake makes these observations independent of timing guesses. Artificial gates
are used only for lifecycle checks and are excluded from the CSV batches. Cancellation after start
is tested at an operation boundary, not by attempting to interrupt a hash or system call. This is
not a test of fetch validation's separate cooperative cancellation checks.

The worker owns its permit and an `Arc` retaining the temporary fixture. The driver retains and
joins all supervisor handles, even when their response receiver disappears. Failed response delivery
is expected after drop. Successful completion assertions check zero outstanding work and returned
permits. Fixtures are removed after all their work finishes. Panics fail the experiment; this is not
a production scheduler with durable failure reporting or shutdown recovery. No writes/publication
are measured, and no rollback or stopped-write guarantee is implied.

## Historical compiler evidence

At experiment revision `a35f3005`, the probe compiled real `HttpFetch` and `SshFetch`, their
`receive_*` functions and `KnownHistory::new`. Inputs included a selected ID, remote and verified
history. No network call executed. [handoff.txt](handoff.txt) retains that run's results:

- Passing a borrowed download into `spawn_blocking` failed with `E0521` (borrow escapes).
- Borrowing `&Arc<KnownHistory>` and moving another Arc alongside the result failed with `E0597`:
  keeping the allocation alive did not extend the compiler-visible borrow to `'static`.
- Moving remote/history ownership into a worker, then downloading with `Handle::block_on` and
  validating there, compiled but occupied a worker during network waits.
- `std::thread::scope` compiled but synchronously joined, blocking its caller.

These failures motivated the ownership revision. The original probe source remains in that revision;
the current script no longer expects failures from superseded signatures. Both old fetch structs
owned their bytes and negotiation state but borrowed history; `Send + Sync` alone did not meet
Tokio's `'static` handoff bound. A byte-only external wrapper could not reproduce that private
negotiation/validation association.

## Resolved owned handoff

The existing `receive_http` and `receive_ssh` APIs now take `Option<Arc<KnownHistory>>`. Their
results own that Arc privately, keeping the exact negotiated history available until validation
finishes or the download is dropped. No whole-history clone is needed. `None` requests a full
transfer without requiring or retaining a history allocation. Validation takes no replacement
history and returns the existing owned `ReceivedFetch`; installation continues to recheck
known-local dependencies.

[check_handoff.py](../../examples/scheduling/check_handoff.py) now requires both actual transport
results to compile as `Send + Sync + 'static`, including network download followed by
`spawn_blocking` validation after dropping the caller's Arc, and a no-history handoff. The probes
check types only. The HTTP/SSH integration tests execute those handoffs against disposable Git
servers for full, incremental and known-only fetches, observe history release, and reject
installation into a destination missing dependencies. Cancellation and decode-limit failures release
history; discarding a download releases it without validation.

[http_local.rs](../../examples/http_local.rs) and [ssh_local.rs](../../examples/ssh_local.rs) show
caller-owned semaphore admission after async download, worker-owned permits, joined completion and
explicit installation outside the executor. Each submits one request, bounding the queue as well as
running work. Services need their own queue and aggregate byte budgets. Retain completion ownership
even if cancellation is requested: dropping a Tokio blocking-task handle does not stop started work.
Network deadlines end at download; validation still checks cancellation cooperatively and cannot
interrupt a single hash, inflate or parse. Installation retains its existing partial-write contract.

## Recommendation and decisions

Keep local storage, graph computation and validation synchronous internally. Consumers should own
whole-operation scheduling for now. The measured responsiveness benefit requires offloading; it does
not require a library async operation layer. A shared facade can follow if consumers actually need
repeated admission/completion machinery. Async filesystem signatures alone would leave pack
verification, decoding and graph computation on the executor unless separately scheduled.

Resolved choices and remaining consumer policy:

- **Ownership API:** replace the unreleased borrowed signatures with optional owned shared history;
  update in-repository callers together, preserving negotiation/history identity.
- **Scheduling owner:** recommend consumer-managed admission initially; choose a library facade only
  if a concrete consumer benefits from its policy and dependency contract.
- **Drop/cancellation contract:** recommend explicit cancellation requests plus observed completion.
  Decide whether queued cancellation removes work promptly and whether dropping a requester signals
  cancellation. Running history remains non-interruptible without an additional API change.
- **Resource budgets:** choose running, queued and retained-byte limits from the consumer workload.
  This trace supplies no universal limit; a job-count bound alone does not bound memory.

Limits: one machine, one synthetic shape, eight operations per batch, fixed ordering, no confidence
intervals, no cold-cache guarantee, no RSS/retained-byte measurements, no remote network benchmark,
no write/installation/publication cancellation, and no Linux/Windows runtime evidence. The
historical handoff workarounds are compiler evidence only. The ownership revision does not add a
general async filesystem/storage trait or library-managed worker pool.

## Original experiment validation

The release example completed all 16 batches and four lifecycle scenarios. Both transport probes
produced the expected lifetime errors and compiled both workarounds. `cargo test --all-features`,
`cargo clippy --all-features --all-targets -- -D warnings`, `just docs-rs`, `just fmt-check`, and
`cargo check --no-default-features` passed without warnings. Markdown was checked with rumdl and
markdownlint-cli2 using the existing 100-column user configuration. No public library code changed.

## Ownership revision validation

Ownership revision validation passed on macOS arm64: `just check` (681 unit tests, 301 integration
tests, 12 doctests, all-target/all-feature Clippy and docs.rs), both disposable transport examples,
the current compiler probes, core-only compilation, warning-denying all-feature private Rustdoc, and
Markdown checks. No new scheduling timings were collected and no Linux/Windows runtime validation
was performed for this revision.
