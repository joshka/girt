# Contributing to girt

girt is being built incrementally as an idiomatic Rust Git library. Keep contributions focused on
one useful capability and explain its supported behavior and limitations. Discuss substantial API or
architecture changes before implementing them.

For unreleased prototype APIs, prefer one coherent API over preserving superseded signatures. Update
in-repository callers together instead of adding compatibility wrappers or a deprecation period for
consumers that do not exist. Keep the revision scoped to the agreed capability; this policy does not
authorize unrelated API expansion.

## Setup

Install a stable Rust toolchain supporting edition 2024, plus the development tools:

```sh
rustup component add clippy
rustup toolchain install nightly --component rustfmt
cargo install just --locked
cargo install rumdl --locked
cargo install cargo-docs-rs --locked
```

Nightly is needed for rustfmt's unstable options and the docs.rs check. Normal builds, tests, and
Clippy use your default Rust toolchain. Install Vale with your OS package manager for the prose
check (`brew install vale` on macOS).

## Development Checks

For Markdown-only changes, run `just docs-check` (rumdl and Vale). Vale checks spelling and selected
Git terminology in the README, this guide, and the repository, configuration, reference, HTTP, SSH,
and working-tree guides. Its [project vocabulary](.config/vale/config/vocabularies/Girt/accept.txt)
accepts domain terms; review a warning in context before changing a technical contract. Use
`just fmt-md` to fix formatting. Vale does not check historical evidence or Rustdoc. For
Rustdoc-only changes, run the checks applicable to the change in
[Rustdoc Standard](docs/rustdoc.md#examples-and-validation). Corrections that preserve meaning,
links, and examples need prose review, but no Rust tests or documentation builds.

For Rust implementation changes, run these checks before submitting a contribution:

```sh
just fmt
just check
```

`just check` runs formatting checks, tests (including doctests), Clippy across all targets with all
features enabled, and `cargo +nightly docs-rs`. Clippy and Rustdoc warnings fail the checks. Run
`just test`, `just clippy`, or `just docs-rs` for individual checks. The
[docs.rs check](https://docs.rs/about/builds) uses the crate's docs.rs metadata to approximate the
hosted build; it does not reproduce the hosted sandbox.

Use `just fmt-rust` or `just fmt-md` to format one language, and `just fmt-rust-check` or
`just fmt-md-check` to check it without changes. Markdown prose wraps at 100 characters; table
columns and separators stay aligned, even when a table must be wider.

For performance-sensitive changes, run `just bench`. See
[performance measurements](docs/benchmarks.md) for workloads, measured operations, cache conditions,
and recorded results. Changes touching fetch, object reading, packing or retention also need the
[large-repository benchmark](docs/jj-replacement-status.md#large-repository-benchmark).

The [platform workflow](.github/workflows/validation.yml) repeats runtime and interoperability
checks on macOS and Linux, with a separate Windows portable-test job. Its explicit
[integration selection](docs/testing.md#native-platform-coverage) covers storage, objects,
repository operations, remote mapping, references, and HTTP runtime suites. See
[platform evidence and gaps](docs/compatibility.md#platform-and-git-version-validation) before
making support claims. Local cross-compilation does not replace executing the test suite on the
target OS.

CI runs on pull requests and on pushes to `main`. It calls the index, references and security
workflows when a change touches their paths. Changes that touch only Markdown, `docs/`, licenses,
`.vale.ini`, `.config/rumdl.toml` or `.config/vale/` run just the `Docs` job (`just docs-check`);
the MSRV and platform test jobs report as skipped. Any other path runs them. Its `required` job is
the single merge gate: it fails when any other CI job fails or is cancelled, and treats skipped jobs
as passing. Add new CI jobs to its `needs` list.

## API Compatibility Gate

The `Semver` job checks Rust changes against the latest non-yanked stable release on crates.io with
all features. The checker permits compatible additions and deprecations (`--release-type minor`)
while rejecting its major-level API lints, including on 0.y releases. Package version bumps cannot
waive violations. When that comparison detects a break, a second comparison against the first parent
of the merge commit checked by CI distinguishes new breaks from breaks already merged for the next
release. Only new breaks add the `breaking` label and fail `Semver policy` unless a maintainer has
applied `next-breaking`. Unrelated pull requests can pass during a breaking-release batch. Both
comparisons must finish successfully; tool, download, and build errors always fail and cannot be
waived with a label.

Batch deliberate breaks for the next breaking release; after adding `next-breaking`, rerun the
failed policy job. The job fetches current PR labels on each attempt. Remove a stale `breaking`
label after a later update resolves the break. The gate follows cargo-semver-checks' deny-level
lints, including removed APIs and lost auto traits; it does not establish behavioral compatibility.
Rust caches are saved only on main, so PR builds cannot crowd out the shared baseline cache.

The check uses the existing CI concurrency group and both jobs feed `required`. A separate job
writes labels without checking out or building PR code. Fork pull requests receive the label from
the trusted `Label semver breaks` workflow after CI finishes; its workflow must be present on the
default branch. The compatibility gate covers Linux with all features; platform-specific API
differences still need review and the existing platform checks.

## Fuzzing

The [`fuzz/`](fuzz) crate holds cargo-fuzz (libFuzzer) targets for parsers of untrusted input. Each
target calls a bounded entry point in `girt::fuzzing`, a module compiled only under `cfg(fuzzing)`.
A panic, unbounded allocation or hang is a finding. The crate is excluded from the published package
and needs a nightly toolchain:

```sh
cargo install cargo-fuzz --locked
fuzz/seed.py              # seed fuzz/corpus from repositories built by the installed Git
fuzz/run.sh 60            # fuzz every target for 60 seconds
fuzz/run.sh 600 pack      # fuzz one target for longer
```

`fuzz/seed.py` builds SHA-1 and SHA-256 repositories with the installed Git and frames their bytes
for each target; its header comment records seed provenance. `fuzz/run.sh` applies libFuzzer timeout
and RSS limits and writes failing inputs to `fuzz/artifacts/<target>/`. On a shared machine, run
campaigns longer than a minute under the timing lock that the [quality scorecard](docs/quality.md)
uses:

```sh
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
trap 'rm -f /tmp/girt-timing.lock' EXIT
fuzz/run.sh 3600
```

Reproduce a failure with `cargo +nightly fuzz run <target> <artifact>`. Fix it with a regression
test in the ordinary test suite that does not depend on the fuzz crate. The
[fuzz workflow](.github/workflows/fuzz.yml) runs `fuzz/run.sh 60` nightly and on manual dispatch,
uploading any failing inputs. It is not a pull request gate.

## Implementation and Tests

Each issue states its acceptance criteria. [Testing and Completion Criteria](docs/testing.md)
describes the evidence a change needs; unit tests, integration tests, and benchmarks should address
the change's contracts and risks.

Organize modules under `src/` around coherent Git concepts. Prefer readable control flow, explicit
side effects, and types that protect meaningful invariants. Document public API contracts and add
Rustdoc examples when an API becomes usable.

Follow [Documentation Standard](docs/documentation.md) when writing prose and
[Rustdoc Standard](docs/rustdoc.md) for API contracts and examples. Both apply when documenting
implementation changes.

Keep unit tests beside the implementation. Add integration tests under `tests/` and independently
generated fixtures under `tests/fixtures/` as needed. Name tests after behavior, such as
`rejects_truncated_object`.

For Git functionality, test interoperability with Git, malformed inputs, and byte-level round trips
where applicable. Record the Git version and fixture generation steps so compatibility evidence can
be reproduced. Tests must use isolated temporary repositories rather than modify a contributor's
checkout or global Git configuration. Unsupported features should fail explicitly.

## Originality and Licensing

Contributions must be original and suitable for distribution under `MIT OR Apache-2.0`. Do not use
Git, gix, or libgit2 implementation or test source, including comments, as input to girt code,
documentation, or tests. Work from format specifications, consumer requirements, and independent
observations of the Git executable. Record specification references and fixture provenance, and
check dependency licenses before adding them.

## Pull Requests

Explain the problem, resulting behavior, validation performed, and remaining limitations. Link
relevant issues and call out public API changes, new dependencies, and compatibility assumptions.
Keep documentation consistent with what the library actually implements.

## Releases

The first `girt` release was published manually. Subsequent releases use
[`release-plz`](https://release-plz.dev/docs/github/quickstart): a push to `main` prepares a release
pull request, and merging that request permits the release job to publish through crates.io trusted
publishing. Review the proposed version, changelog, CI results and supported scope before merging.

The release workflow uses GitHub's default token, so checks on the release pull request do not start
on their own: GitHub holds them for approval. Approve the waiting runs, or close and reopen the pull
request, after its last update, then wait for `required` before merging. The publish job uses a
short-lived crates.io token obtained through OIDC; do not add a persistent registry token to the
workflow.

## HTTP Fixtures

The optional `http` feature uses a caller-owned Tokio runtime. Its interoperability tests need Git,
Python 3 and OpenSSL on PATH and use only disposable loopback servers. `just check` enables this
feature; also run `cargo check --no-default-features` when changing feature gates. See
[HTTP setup and contracts](docs/http.md) for the example, TLS fixtures and async processing
boundary.

## SSH Fixtures

The optional `ssh` feature uses system OpenSSH and a caller-owned Tokio runtime on macOS/Linux. Its
loopback interoperability tests require Python 3, Git, ssh, ssh-keygen and an unprivileged sshd that
can authenticate the current OS user with a temporary key and a forced command. No persistent
SSH/account configuration is changed. Missing or restricted sshd is a concrete fixture failure, not
a skipped compatibility result. See [SSH contracts and setup](docs/ssh.md).
