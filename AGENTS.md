# Repository Guidelines

## Purpose

`girt` is an idiomatic Rust Git library. Its first consumer is jj; an experimental jj branch already
runs on it. Performance and fidelity to Git are the priorities.

- Model Git's concepts directly: objects, trees, commits, references, the index, and repositories.
- Preserve Git's formats and semantics exactly for supported features. Reproducing Git's CLI
  behavior or another library's API is not the goal.
- Report unsupported cases explicitly with an error; never approximate them silently.
- Data loss, corruption, unsafe mutation and failures of core required interoperability are
  blockers. Other edge-case differences may ship as explicit refusals or documented limitations when
  fixing them would materially delay the task; open an issue for each one.
- Build narrow, usable increments; avoid speculative abstractions or configuration complexity.
- Design Git APIs from first principles; consumer call sites establish requirements, not method
  names or architecture to copy.

## Work Tracking

Work comes from [GitHub issues on joshka/girt](https://github.com/joshka/girt/issues). Each issue is
one task with its own acceptance criteria and ends in one small pull request. Read
[jj on girt status](docs/jj-replacement-status.md) for the consumer contract, the large-repository
benchmark and the build and acceptance workflow. The [roadmap](docs/jj-roadmap.md) and
[acceptance matrix](docs/jj-acceptance.md) are historical records of the earlier phase, not the
tracker.

## Independent Implementation and Licensing

- Target dual MIT/Apache-2.0 licensing. Write original code, documentation, and tests.
- Do not use upstream Git, gix, or libgit2 implementation or test source as implementation input,
  including copyright-audit comparisons. Use specifications, jj call-site requirements, and
  independently generated observations of the Git executable.
- Establish compatibility through format specifications and observable behavior; record reference
  and fixture provenance.
- Check dependency licenses before adding dependencies.

## Guides

Load the guide that matches the work:

- Rust design, implementation and review: [Rust Conventions](docs/rust-conventions.md).
- Tests, Git compatibility and benchmarks: [Testing and Completion Criteria](docs/testing.md).
- Anything that deletes, rewrites or re-encodes stored data, adds a fast path or optimizes a
  benchmarked path: [Specify Before Implementing](docs/testing.md#specify-before-implementing)
  before writing code.
- Any prose, including doc comments and pull request descriptions:
  [Documentation Standard](docs/documentation.md).
- Public APIs, Rustdoc and examples: [Rustdoc Standard](docs/rustdoc.md).
- Setup and which checks to run: [Contributing](CONTRIBUTING.md).

When maintainer feedback establishes a reusable rule, add it with its rationale to this file or the
relevant guide as part of the work.

## Commands

- `just check`: formatting checks, tests, Clippy and the docs.rs build, all warning-free.
- `just fmt`: format Rust (nightly rustfmt) and Markdown (rumdl).
- `just fmt-md-check`: check Markdown formatting only.
- `cargo test`, `just clippy`, `just docs-rs`: individual checks.

If tools are missing, follow [Setup](CONTRIBUTING.md#setup).

## Code and Tests

- Add modules under `src/` around coherent Git concepts. Keep unit tests beside the implementation,
  integration tests in `tests/`, and original fixtures in `tests/fixtures/`.
- Test supported behavior against the Git executable, including malformed inputs and byte-level
  round trips where applicable. Generate fixtures independently.
- Keep Cargo and toolchain output warning-free, including manifest and Rustdoc warnings. Fix the
  cause rather than suppressing it; if that is out of scope, report it and open an issue.
- Finish the change, update affected docs, and fix failures it causes before handing it back.
- Run the checks that match the change (see
  [Development Checks](CONTRIBUTING.md#development-checks)); rerun only after further edits or
  failures.

## Pull Requests

- Reference the issue (`Fixes #N`). Each pull request is reviewed by the other agent provider.
- State the problem, the behavior changed, compatibility and benchmark evidence, validation
  commands, and limitations. Call out public API and dependency changes.
