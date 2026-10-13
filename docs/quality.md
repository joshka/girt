# Quality Scorecard

The scorecard measures girt on four axes: correctness, robustness, idiomatic Rust and documentation.
Each metric has a target, so a run shows where the library falls short of its own standards rather
than an overall grade. The standards are the repository guides: [Testing](testing.md),
[Rust Conventions](rust-conventions.md), [Documentation](documentation.md), [Rustdoc](rustdoc.md)
and [Storage Safety](storage-safety.md). Numeric targets the guides do not set, such as 85% line
coverage, 70% branch coverage, 40% of items with examples and 90 per axis, are scorecard policy
chosen on [#100](https://github.com/joshka/girt/issues/100), not repository conventions; the guides
establish no coverage threshold. The metric definitions and first results are on #100, and each gap
links its own issue from there.

## Running It

```sh
scripts/quality-scorecard.py             # static, Clippy and Rustdoc metrics; a few minutes
scripts/quality-scorecard.py --coverage  # also remeasure line and branch coverage
scripts/quality-scorecard.py --mutants   # also rerun the sampled mutation test
scripts/quality-scorecard.py --full      # both
```

The script prints a Markdown table and writes JSON to `~/tmp/girt-quality.json` (`--output` changes
the path) in this shape:

```json
{"axes": [{"axis": "correctness", "score": 72, "target": 90,
           "metrics": [{"name": "...", "value": 80.6, "target": 100, "status": "gap"}]}]}
```

Coverage and mutation results are slow, so they are cached under `target/quality/`. A default run
reuses the last results and notes when they were measured. A failed tool, compilation or test run
stops the scorecard with its output instead of reporting a result, and the cache is replaced only
after a complete measurement. A mutation run counts only when its unmutated baseline passes and it
tests at least one viable mutant. Every Cargo step waits for `/tmp/girt-timing.lock` and holds it
only while it runs, so timing work on the same machine is not disturbed.

The script needs Python 3.9 or newer, `shlock` (shipped with macOS), the stable and nightly
toolchains and `cargo-docs-rs`. The slow measurements also need `cargo-llvm-cov` (Apache-2.0 OR MIT)
and `cargo-mutants` (MIT), installed with `cargo install`, and the nightly LLVM tools, installed
with `rustup component add llvm-tools-preview --toolchain nightly`. None is a crate dependency.

## How Metrics Are Computed

The script keeps a table of capabilities, such as trees, pack reading, fetch and the index. Each
entry lists its source paths and integration test files, and flags which properties apply: a Git
byte format girt writes, a parser of repository or network input, a bounded reader, a long-running
operation, or a mutation of stored data. Test metrics then check the applicable property for each
capability:

- a Git-oracle test runs the `git` executable or a `*_git` support helper in its integration tests;
- SHA-256 coverage means those tests exercise SHA-256 repositories;
- round-trip, malformed-input, limit, cancellation and crash-ordering coverage come from test names,
  matched against patterns such as `round_trip`, `rejects`, `limit`, `cancel` and `crash`. Each
  named `rstest` case counts as its own scenario, and patterns match only at the start of a name
  component, so `fault` does not match `default`. Crash evidence means an injected failure or
  recovery around publication; refusing bad input before publishing counts as malformed input. Tests
  whose names miss the vocabulary but provide the evidence are listed explicitly in the script's
  `EVIDENCE` table.

Name matching shows where evidence is missing, not that the evidence present is sufficient. A
capability that passes still needs review against [Testing](testing.md). Add a new capability to the
table when it lands, so the scorecard does not silently skip it.

Panic reachability comes from Clippy's `unwrap_used`, `expect_used`, `panic`, `unreachable` and
`indexing_slicing` lints on library code, excluding tests, counted in parser source files. API and
documentation metrics come from nightly Rustdoc JSON. They cover publicly reachable items, their
fields and variants, and inherent methods. Trait implementation members are excluded because their
documentation belongs to the trait.

Mutation testing samples one quarter of the mutants in the object, tree, commit, tag and pkt-line
modules and runs the unit tests and object integration tests against each. Rotate `MUTANT_SHARD` in
the script to cover the rest over time.

An axis score is the mean of its scored metrics. A metric where higher is better attains its value
divided by its target, capped at 1. A count whose target is a maximum attains 1 at or under the
target and `(target + 1) / (value + 1)` above it, so one panic site against a target of 0 attains
1/2. An unmeasured metric, such as coverage before the first `--coverage` run, attains 0 and marks
the axis incomplete, so an unmeasured checkout never outscores a measured one. Clippy pedantic and
nursery counts, the size of the public API and fuzz targets are reported without a score. Fuzzing is
deferred until after release by the [publication plan](publication-plan.md), so the scorecard
reports the gap without counting it.
