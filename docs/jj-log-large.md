# jj full-history log investigation

The `log` / `large` row runs the following command, with output discarded:

```sh
jj log -r 'all()' --no-graph -T 'commit_id ++ "\n"'
```

The Linux v6.1 fixture contains 1,138,794 reachable commits. Despite the ID-only template, jj reads
full commit objects. Its persisted index supplies revset ordering and generation information; this
path does not use girt's commit graph. The template requests neither trees nor diffs.

## Profile and applicable levers

An initial ten-second sample window attributed 6,436 of 7,483 main-thread samples to backend commit
reads, including 5,112 beneath the shared object-read path. Nested samples included 2,050 in
decompression, 1,742 in positioned reads, and 326 in identity verification. These are overlapping
sample counts, not additive CPU percentages or a full-run phase accounting. Rendering accounted for
785 samples. Index and operation-log work did not dominate this window; startup is outside that
conclusion.

The [performance checklist](https://github.com/joshka/girt/issues/599) suggests hash verification
(4), commit graphs/generation numbers (15), pack memory/cache choices (20), and caching (21). Full
IDs make short-prefix lookup irrelevant. Verification was a small sampled share and remains enabled.
Commit-graph acceleration cannot remove the full commit reads required by this jj path.

A bounded compressed-byte cache produced only a small, inconsistent isolated-read improvement. Its
full-command median remained 1.224 times stock, with a memory tradeoff. That experiment was
[closed without merging](https://github.com/joshka/girt/pull/858). Increasing concurrency on the
global Rayon pool also regressed, with substantial context-switch and system CPU overhead.

## Adapter change

The adapter pipelines reads through one lazy dedicated worker, with 32 buffered requests in normal
jj streams. It shares an immutable object snapshot through `Arc`, copying it on refresh. A missing
or wrong-kind result falls back to the existing refresh and checked error path. Calls already
running in Rayon read inline, and failure to create a worker also falls back inline. Worker panics
unwind in the awaiting caller. Object identity verification remains enabled.

The backend concurrency hint affects other consumers too, and is advisory rather than a global queue
limit. External callers can enqueue more requests. Each backend that reads commits adds one worker
thread. The normal stream buffers at most 32 requests; this is not an unbounded commit cache.

The reviewed adapter is
[commit 2dd192df](https://github.com/joshka/jj/commit/2dd192dfdde003448e94634c7970da013226bf71) on
the fork's `joshka/girt-backend` branch. It uses published girt 0.17.0. The measured binary's
SHA-256 is `d5fa7027a032a3588aff462bb36d997fe9a8fe53e56a89c7fefd7fed85188b51`.

## Measurements

The final adapter meets the ≤1.2× stock target in both full Linux rounds. Each cell is the median of
two samples; the medium result is a spot check, not a full matrix refresh.

| Row / round          | Unchanged girt | Candidate |    Stock | Before / stock | After / stock |
| -------------------- | -------------: | --------: | -------: | -------------: | ------------: |
| Linux, round 1       |       13.736 s |  10.896 s | 10.984 s |          1.25× |         0.99× |
| Linux, reverse round |       13.663 s |  10.329 s | 10.786 s |          1.27× |         0.96× |
| Medium spot check    |        0.975 s |   0.766 s |  0.849 s |          1.15× |         0.90× |

Linux candidate peak RSS ranged from 608 to 616 MiB, versus roughly 995–996 MiB for stock. The
pipeline closes the gap without a large commit cache. One-minute load at sample start ranged from
22.1 to 38.3 across the final Linux rounds.

The four-request version did not hold across full-command rounds: one round favored it, but the
reverse-order round measured 14.490/15.727 seconds against unchanged girt's 14.295/15.220 seconds.
That round stopped when load exceeded 40. Increasing the buffer to 32 reduced the isolated phase to
8.599/7.980 seconds against 11.202/10.654 seconds, a 24% median reduction. The final full-command
measurements use that larger buffer.

[Provenance](benchmarks/jj-log-large-provenance.json) and
[raw experiment observations](benchmarks/jj-log-large-experiments.json) retain all these samples,
including the interrupted round. Samples from different adapter versions are not pooled.

Fresh filesystem copies were prepared before acquiring the shared timing lock and removed after
releasing it. Tool order alternated between rounds. Each timed command required a one-minute load
below 40; no cache eviction was performed. Concurrent unrelated activity still caused substantial
variation, so absolute times should not be treated as stable latency guarantees.

The isolated read/render phase (`--ignore-working-copy` with the same log arguments) fell from
11.235/11.241 seconds to 9.905/9.358 seconds in an earlier pipeline build, about 14% by median. Peak
RSS stayed near 585–589 MiB. This phase measurement excluded working-copy startup and used a
snapshot clone per request; the final implementation shares that snapshot instead.

The investigation continues
[the packed commit-read issue](https://github.com/joshka/girt/issues/222).

## Validation

The adapter passes default-feature and all-feature `cargo check --all-targets`, all-feature
all-target Clippy with warnings denied, 25 backend unit tests, and eight focused backend integration
tests. The report workspace also passes both girt check configurations, Clippy, and the six error
unit tests. A cross-provider review identified worker panic propagation, formatting, the shared
concurrency hint, and missing error-path coverage; the implementation addresses those findings. No
hosted adapter CI runs on direct fork-branch pushes, so platform-specific runtime coverage is
limited to the focused local tests.
