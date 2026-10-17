# Quality Scorecard

The scorecard measures girt on five axes: correctness, robustness, idiomatic Rust, documentation and
ergonomics. Each metric has a target, so a run shows where the library falls short of its own
standards rather than an overall grade. The standards are the repository guides:
[Testing](testing.md), [Rust Conventions](rust-conventions.md), [Documentation](documentation.md),
[Rustdoc](rustdoc.md) and [Storage Safety](storage-safety.md). Numeric targets the guides do not
set, such as 85% line coverage, 70% branch coverage, 40% of items with examples and 90 per axis, are
scorecard policy chosen on [#100](https://github.com/joshka/girt/issues/100), not repository
conventions; the guides establish no coverage threshold. The metric definitions and first results
are on #100, and each gap links its own issue from there.

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
nursery counts and the size of the public API are reported without a score. Fuzz coverage counts the
parser capabilities that a [fuzz target](../CONTRIBUTING.md#fuzzing) exercises.

## Ergonomics

The [everyday task catalogue](ergonomics.md) adds a fifth axis. It records 15 workloads, separates
missing capabilities from verbosity, and links proposed improvements to their issues. Current
Markdown snippets are compiled by `scripts/check-guide-examples.py`; linked example programs are
checked by Cargo. Proposed sketches are excluded from compilation and measurement.

```sh
nice -n 10 python3 scripts/quality-scorecard.py --ergonomics-only
```

This fast mode measures the same axis as a full scorecard run. It reports nonblank source lines,
explicitly imported names, default/trusted calls, never-cancel constructors, byte-vector conversions
and explicit object formats, identity literals, commit fields, branch prefixes (including refspecs),
boxed errors and empty vectors, with per-task details in JSON. Counts cover the catalogue's stated
inputs and include shared setup each time; linked fixture programs are excluded. These are lexical
cost indicators, so review scope and formatting changes before interpreting a reduction.

The dated [baseline](ergonomics-baseline.json) records 2026-10-17 against library code `1ecc6fff`;
that revision predates the catalogue. Its catalogue SHA-256 identifies the initial measured text.
The baseline stays fixed. The separate [budget](ergonomics-budget.json) stores per-task maxima; when
an ergonomics change lowers a cost, lower that task's budget in the same PR. The report shows
reductions against the dated baseline and flags every task that exceeds its budget, so an
improvement in one workload cannot conceal a regression in another. Task names must match all three
artifacts, and budgets cannot exceed the dated baseline. The capability-gap target remains zero. The
scorecard reports gaps without failing the process; it is an investigation tool, not a CI gate.

| Caller-cost indicator                               | Improvement issue                                |
| --------------------------------------------------- | ------------------------------------------------ |
| Lines and imported names                            | All catalogue follow-ups                         |
| Default/trusted calls and never-cancel constructors | [404](https://github.com/joshka/girt/issues/404) |
| Signature literals and byte-vector conversions      | [403](https://github.com/joshka/girt/issues/403) |
| Branch prefixes and repeated object formats         | [405](https://github.com/joshka/girt/issues/405) |
| CommitFields literals and empty vectors             | [406](https://github.com/joshka/girt/issues/406) |
| Boxed heterogeneous errors                          | [407](https://github.com/joshka/girt/issues/407) |

Counts are lexical indicators. A zero can mean the supplied inputs already include an identity or an
object ID; it does not prove that an application can construct that input without extra code. The
per-task input contract must remain comparable when updating a snippet and lowering its budget.
