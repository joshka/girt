# jj operation matrix

`bench-ops.py --matrix` compares independent operations using fresh APFS `cp -cR` copies for
mutations. Build both binaries before timing. Keep fixtures, builds, and copies under
`~/tmp/girt-bench` or `~/tmp/girt-fixtures`; only the shared timing lock lives in `/tmp`.

The default method uses Git, girt, stock, stock, girt, Git blocks, with six samples per tool. For
expensive repositories, add `--adaptive --bounded-disk`:

1. Measure Git, stock jj, then girt once to calibrate the cell.
1. If the slowest first sample exceeds 60 seconds, retain one sample per tool. If it exceeds 10
   seconds, collect three. Otherwise collect six, alternating girt and stock positions.
1. Cap each girt sample at `max(2 * first stock-jj seconds, 5 seconds)`. Cap stock jj and Git at 600
   seconds per sample. Kill the complete timed process group on timeout.
1. Record a timeout as a failure with its limit, never as a successful timing or ratio. Report
   successful sample counts and attempted runs separately for every tool.
1. Flag stock jj taking more than twice Git's median as a jj-level gap. This flag does not fail the
   girt cell; commands with different semantics still require interpretation.

Resume preserves completed measurements, including older six-sample cells and partial samples. A
partial cell can already satisfy the adaptive target. Refusals remain explicit failures with no
successful girt ratio. Record the reason and issue link rather than fabricating samples.

Hold `/tmp/girt-timing.lock` only while timing; release it during setup and validation. With
`--bounded-disk`, prepare one copy at a time, checkpoint timing and validation, then remove the
copy. Retain raw JSON, logs, fingerprints, fixture facts, and profile evidence. Full `git fsck`
checks follow `--fsck-scope`; `operation-type` also checks clone and explicit GC at each size.
Adaptive diagnostic profiles record at most five seconds and do not replace timing samples.

Before a release, rerun the matrix on a quiet machine with the intended frozen source revisions,
then regenerate `docs/jj-performance.md` from `docs/benchmarks/jj-matrix.json`. CI checks the
generated page for freshness; it must not run the benchmarks. Include the actual method and sample
counts when publishing results, because adaptive slow-cell ratios have lower precision.

The committed JSON uses schema version 1: an object with `schema_version`, `provenance`, and
`results`. Each result includes `repo`, `size`, `op`, `variant`, `girt_ms`, `stock_ms`, `git_ms`,
and per-tool `*_samples`, `*_attempts`, and `*_rss_mb` fields. Times are medians in milliseconds;
RSS is peak MiB. A missing equivalent or unavailable successful measurement is `null`. `status` is
`ok`, `fail`, or `timeout`; a refusal is `fail` with `failure_kind: "refusal"`. Rows also retain
ratios, notes, issue links, fingerprints, and measurement epochs. The provenance block identifies
builds, compatibility patches, machine, dates, fixture layouts, and method. Consumers should use
field names rather than row order.

Fetch setup performs an untimed fetch before adding any measured commits or tags, so noop means a
second fetch with no remote changes. Limited history uses `jj log -r ::@ --limit 100` against
`git log -n 100`; a parent-distance revset such as `ancestors(@, 100)` does not limit the number of
output revisions.

The schema above describes the **published** files, not the runner's direct output. The runner
writes a timing list plus a raw checkpoint; successful rows can omit `status`, and refusals use
`unsupported`. Publication currently requires manual normalization and diagnosis:

1. Merge selected reruns by repository and operation, retaining superseded records separately.
1. Normalize missing success status to `ok` and `unsupported` to `fail` with
   `failure_kind: "refusal"`; preserve timeouts without a successful ratio.
1. Derive per-tool ranges, attempts, statuses, and fingerprints from the raw checkpoint. Attach
   reviewed issue mappings and retained profiles; distinguish timing and profile build epochs.
1. Audit all intended cells and validation coverage. Operation-type fsck can reuse earlier checks
   across binary revisions; report that limit rather than claiming fresh validation of every rerun.
1. Update the envelope and its identical list/provenance mirrors at `docs/jj-performance.json` and
   `docs/jj-performance-provenance.json`, then run the renderer and `--check`.

The 2026-10 publication retains local merge/normalization scripts and raw checkpoints as evidence;
those scripts are not a committed end-to-end publication command. Review the normalized data before
each release. CI checks consistency and rendering only, not measurement validity.
