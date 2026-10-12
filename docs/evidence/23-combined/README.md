# Combined delta-reuse evidence

This records the initial and review-fixed 2026-10-11 local measurements of the combined
implementation on macOS 26.6.2, Apple M2 Max (96 GiB), Rust 1.99.0, Git 2.56.0, and APFS. The input
is the original bare `jj-vcs/jj` fixture at `/tmp/opus-med-23/source.git`, HEAD
`987c084b8f3ff28e8246d8b38c55c323c85611d5`, containing 131,152 packed objects.

## Reproduction

Run from the combined jj workspace with an empty `/tmp/23-combined-bench` output directory and the
original fixture available. The scripts use the exact quiet driver supplied for the comparison.
Comparison sources are exported read-only from the recorded commit IDs; builds happen before
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

`export.py` installs the supplied driver, the additional `stages.rs` binary, and both timing scripts
into the output directory. The builds above include the combined driver, scan profiler and
recent-pack driver. Then run:

```sh
zsh /tmp/23-combined-bench/run.sh
zsh /tmp/23-combined-bench/workloads.sh
```

Both shell scripts take `/tmp/girt-timing.lock` for their entire block and record uptime at both
ends. Comparison runs use a fresh `cp -cR` clone for each command. The supplemental workloads use
one fresh clone per three-run sequence, adding a new root before each run, to measure changing roots
and protected-pack accumulation. Those sequences intentionally retain prior output. The recent-pack
sequence touches the input pack and uses a cutoff two weeks in the past, exercising recent packed
roots and their fully decoded dependency closure.

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

## Reviewed implementation results

The reviewed production code is `e0b15abf`; measured head `408dc831` adds only the Unix gate on a
fixture helper. Both judges approve that head. All 24 comparison commands succeeded, with a fresh
APFS clone for each command and one shared lock for the complete rotated block.

| Candidate                                                  |    Repack s (range) | Max RSS MiB |  Maintenance s (range) | Max RSS MiB |
| ---------------------------------------------------------- | ------------------: | ----------: | ---------------------: | ----------: |
| Combined                                                   |    7.38 (7.29–7.48) |       407.0 |    51.63 (51.26–52.68) |       452.9 |
| [#47 (Opus low)](https://github.com/joshka/girt/pull/47)   | 19.34 (18.55–19.40) |       410.3 | 124.41 (122.96–125.27) |       441.2 |
| [#49 (Sol medium)](https://github.com/joshka/girt/pull/49) |    8.26 (8.02–8.28) |       416.7 |    60.60 (60.23–60.96) |       466.0 |
| Git 2.56.0                                                 |    1.43 (1.40–1.43) |       284.0 |       1.53 (1.52–1.55) |       285.0 |

RSS is the maximum across three `/usr/bin/time -l` readings. Raw records, uptime, binary hashes,
validation and output integrity checks for the reviewed implementation are in `review-fixed/`. The
earlier measurements below are retained as historical evidence.

Recent packed IDs are traversal roots to preserve their dependency closure. This increases scan cost
when a large pack is recent; the supplemental touched-pack sequence measures that separately. A
recent retained object whose dependency is already missing makes planning refuse maintenance. Git
2.56 GC can retain that incomplete object and continue. Girt deliberately retains the stricter
complete-walk requirement before authorizing deletion; this is a compatibility limitation.

## Supplemental workload results

Each sequence starts from one fresh clone, adds one blob tag before each run, and retains prior
outputs. Times are three-run medians and ranges; RSS is the maximum peak reading. These sequences
measure successive changes, rather than three identical inputs.

| Workload                        |    Repack s (range) | RSS MiB | Maintenance s (range) | RSS MiB |
| ------------------------------- | ------------------: | ------: | --------------------: | ------: |
| Changing roots                  |    7.38 (6.92–7.78) |   406.0 |   51.19 (51.01–51.52) |   465.6 |
| Protected source (`.keep`)      |    7.47 (7.01–7.90) |   420.3 |   62.47 (62.45–63.35) |   434.3 |
| Recent source (two-week cutoff) | 13.68 (13.62–14.09) |   425.1 | 102.14 (99.76–102.29) |   488.1 |

Additive repack intentionally retains earlier generations: all three sequences end with four packs,
totalling 822,795,236 bytes. Maintenance with ordinary changing roots ends with one 205,698,848-byte
pack. Protected maintenance ends with the protected source and a 110-byte pack containing the three
new roots, totalling 205,698,880 bytes. Recent maintenance keeps the source plus three recent
58-byte packs, totalling 205,698,944 bytes. Neither retirement sequence adds a full-size duplicate.

The old-pack comparison meets the eight-second repack target and remains below 60 seconds for
maintenance. Protected and recent maintenance miss that target; recent repack also exceeds eight
seconds. Full recent-ID traversal increases scan cost, and protected omission reads the survivor
both before publication and before unlinking. The retained integrity requirements take precedence
over avoiding those reads. Repeated phase observations and recent-root traversal remain performance
work for [#24](https://github.com/joshka/girt/issues/24).

## Initial results (before review fixes)

These historical results apply to `2b6a46cb`, before the contract and dependency-closure fixes.

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
under” target. These figures precede the review fixes; the reviewed comparison and supplemental
workload results are reported above. Integrity and amplification regressions cover those workloads.
