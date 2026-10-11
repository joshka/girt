# Combined delta-reuse evidence

This records the 2026-10-11 local measurements of the combined implementation on macOS 26.6.2, Apple
M2 Max (96 GiB), Rust 1.99.0, Git 2.56.0, and APFS. The input is the original bare `jj-vcs/jj`
fixture at `/tmp/opus-med-23/source.git`, HEAD `987c084b8f3ff28e8246d8b38c55c323c85611d5`,
containing 131,152 packed objects.

## Reproduction

Run from the combined jj workspace with an empty `/tmp/23-combined-bench` output directory and the
original fixture available. The scripts use the exact quiet driver supplied for the comparison.
Comparison sources are exported read-only from the recorded remote bookmarks; builds happen before
acquiring the shared timing lock.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
unset RUSTUP_TOOLCHAIN
python3 docs/evidence/23-combined/export.py
for label in opus-low sol-medium combined; do
    cargo build --release --manifest-path /tmp/23-combined-bench/$label-driver/Cargo.toml \
        --target-dir /tmp/23-combined-bench/target
done
```

`stages.rs` is an additional binary in the combined driver package. Build it before timing, then
copy `measure.py`, `run.sh`, `workloads.py`, and `workloads.sh` into `/tmp/23-combined-bench` and
run the two shell scripts. Each takes `/tmp/girt-timing.lock` for its entire block and records
uptime at both ends. Comparison runs use a fresh `cp -cR` clone for each command. The supplemental
workloads use one fresh clone per three-run sequence, adding a new root before each run, to measure
changing roots and protected-pack accumulation. Those sequences intentionally retain prior output.

The comparison rotates four candidates across three rounds. `/usr/bin/time -l` records elapsed
seconds and maximum resident set size; process RSS is separate from logical payload limits. Git uses
`repack -ad` and `gc --prune=now`. The driver uses trusted limits and a recent cutoff one minute in
the future so fixture packs are eligible for retirement. These are warm-cache observations, not
cold-storage guarantees.

## Integrity and validation

The retained integration suites use original Git-verified fixtures for damaged entries, mislabelled
bases, source precedence, combined depth, protected copies, recent extras, and copied byte budgets.
Both hash formats have byte-identical comparisons with `git repack -adf` and delta-count checks with
`git verify-pack`. Pack units exercise malformed zlib, framing, offset boundaries and source
truncation. The retirement fault model checks ordering if an unsynchronized index unlink is lost
while the later pack unlink persists; it does not establish physical power-loss behaviour.

## Results

Seconds are medians of three rotated runs, with the full range in parentheses. RSS is the largest
maximum resident set size recorded across those runs.

| Candidate                                                  |    Repack s (range) | Max RSS MiB |  Maintenance s (range) | Max RSS MiB |
| ---------------------------------------------------------- | ------------------: | ----------: | ---------------------: | ----------: |
| Combined                                                   |    7.41 (7.17–7.69) |       388.2 |    51.46 (51.35–54.20) |       428.4 |
| [#47 (Opus low)](https://github.com/joshka/girt/pull/47)   | 18.99 (18.74–20.39) |       413.4 | 126.28 (125.18–133.03) |       459.8 |
| [#49 (Sol medium)](https://github.com/joshka/girt/pull/49) |    9.01 (8.23–9.26) |       418.5 |    59.50 (59.32–63.41) |       448.0 |
| Git 2.56.0                                                 |    1.58 (1.45–1.61) |       288.0 |       1.55 (1.55–1.69) |       284.3 |

Every comparison command succeeded. The girt variants produced identical checksum-named packs of
205,698,770 bytes; Git produced 205,651,818 bytes (0.023% smaller). The combined output contains all
131,152 objects, including 98,715 deltas with maximum depth 50.

The three separately timed complete scans took 6.43, 6.50 and 6.42 seconds. Profiled end-to-end
repack was about 7.49 seconds: full object decoding and traversal dominate; reopening sources,
copying, index writing, and publication account for the remainder. Maintenance invokes separate
expiry, retirement and pruning observations and reads the replacement back before deletion. Those
checks explain why repack's speedup does not make the entire workflow as fast as Git. Further
reduction of repeated phase scans belongs to
[the retention performance issue](https://github.com/joshka/girt/issues/24).

The 51.46-second maintenance median is below 60 seconds, with less headroom than the requested “well
under” target. Supplemental changing-root and protected-pack timing is pending the shared lock
priority for the other active validation. Integrity and amplification tests for those workloads
already pass.
