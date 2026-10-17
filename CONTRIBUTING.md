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

The [platform workflow](.github/workflows/validation.yml) runs full Linux validation on ordinary
pull requests: tests, feature checks, examples, Clippy, Rust formatting, docs.rs and API
compatibility. Index and References checks also run on Linux when their paths change. macOS and
Windows coverage runs after pushes to `main`, nightly, on release-plz pull requests, and on pull
requests labeled `full-ci`. Main and nightly checks cover the whole tree. The native test commands
and Windows feature lanes are the same in those full runs. See
[platform evidence and gaps](docs/compatibility.md#platform-and-git-version-validation) before
making support claims.

### Start-and-repair readiness

girt has no downstream consumers yet. A break on main is cheap to repair, so hosted CI runs in the
background while reviewed changes continue. Mark a pull request ready once the relevant local checks
pass and an independent cross-provider review is posted and addressed. Use `just check` for Rust
changes, or the relevant subset described above for narrower changes, and record the commands,
results and checked revision. Do not wait for hosted CI to post readiness or block unrelated work
behind a slow job. Report pending checks and any known failures honestly.

An administrator may merge a ready pull request with `gh pr merge --admin --squash`, matching the
reviewed head with `--match-head-commit`. The main ruleset still requires `required` for ordinary
merges and retains its existing administrator bypass through pull requests. No repository rule is
relaxed: the administrator chooses start-and-repair using local evidence and review. Reverting this
policy only requires returning to ordinary merges. Known data-loss, corruption, unsafe mutation or
core interoperability failures remain blockers.

When a change can affect the published API, the semver policy below still applies before an
administrator bypasses hosted CI: use completed Semver evidence for the current head, or run the
same published-release and PR-base comparisons locally and record the results. A new break requires
explicit maintainer approval through `next-breaking`; a tool/build error is never evidence of
compatibility. Without this evidence the pull request is not ready. A label cannot waive a failed
Linux publish check.

For local API evidence, install the same checker as CI:

```sh
cargo +stable install cargo-semver-checks --locked --version 0.51.0
cargo +stable semver-checks -p girt --baseline-version "$PUBLISHED_VERSION" --release-type minor --all-features
cargo +stable semver-checks -p girt --baseline-rev "$PR_BASE" --release-type minor --all-features
```

Set `PUBLISHED_VERSION` to the latest non-yanked stable girt release on crates.io and `PR_BASE` to
the target-branch commit used for the tested merge. Run from that tested merge revision; CI uses its
first parent for `PR_BASE`. The second comparison is needed only if the published-release comparison
reports a break. Exit 100 means a detected break; any other nonzero exit is a tool or build failure.
Record both baseline revisions and results. Main's published-release comparison is advisory because
it includes approved breaks awaiting release; it cannot detect a maintainer bypassing this readiness
policy. The coordinator must verify that evidence before merging.

Apply `full-ci` for platform-sensitive changes: filesystem operations, locks, paths, permissions,
process execution, Windows code, platform-dependent dependencies or CI workflows. Apply it when
uncertain whether Linux establishes the contract. This requests background evidence; it does not add
a readiness wait. Keep the label on a failing change and repair its failure. Release branches named
`release-plz-*` always request the full matrix. Label changes start another CI run.

Ordinary Rust pull requests run all-feature tests, Clippy and Rust formatting on Linux, plus MSRV,
API compatibility and path-selected Index, References and Security jobs. Feature permutations,
examples and documentation builds run on main, nightly and full-matrix pull requests. No test shards
are added. Documentation-only changes run `Docs` (`just docs-check`); Rust and platform tests skip
even with `full-ci`. Changes under `site/` run only the `Site` workflow and, for Markdown, `Docs`.
The `required` job aggregates failures and cancellations, treating skipped jobs as passing; include
every new job in its `needs` list. Rust caches are saved only on main.

A trusted completion workflow updates one `CI regression on main` issue with failing jobs and run
links after main or nightly CI. It reopens the issue on a new failure and closes it on recovery. New
failures during an open incident also post a comment to preserve details and notify subscribers.
Every eligible completion reconciles the newest push/nightly CI run, so delayed old events still
report the current result. A newer run still in progress delays that update until it completes.
Contributors and the coordinator should claim and repair failures promptly, add a regression check
where appropriate, and publish a patch if a released version is affected. Repairs are small reviewed
changes; unrelated ready work continues. A workflow that never starts cannot report itself and needs
the coordinator's health check.

## Website

The project website in `site/` is an [Astro Starlight](https://starlight.astro.build/) site,
published to GitHub Pages from `main` by `.github/workflows/site.yml`. It complements docs.rs with
concepts, task guides, the compatibility and safety model and the jj benchmark. API details stay in
Rustdoc, and site pages link to them. Node and pnpm versions are pinned in `site/.node-version` and
`site/package.json`. To work on it:

```sh
cd site
pnpm install
pnpm dev          # local preview at http://localhost:4321/girt/
pnpm check        # type-check
pnpm build        # build, checking internal links
pnpm check:links  # after a build: external and docs.rs links, including anchors
```

The performance page renders `docs/jj-performance.json` and `docs/jj-performance-provenance.json` at
build time; update those files rather than editing numbers in the site. Commit no images directly:
store any image with Git LFS (see `.gitattributes`), or prefer SVG or HTML generated at build time.

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

The check uses the existing CI concurrency group and both jobs feed `required`. Administrator merges
must satisfy the same policy with recorded local evidence when CI is pending. A separate job writes
labels without checking out or building PR code. Fork pull requests receive the label from the
trusted `Label semver breaks` reusable workflow through `Main CI health` after CI finishes; both
workflows must be present on the default branch. The compatibility gate covers Linux with all
features; platform-specific API differences still need review and the existing platform checks.

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
pull request and independently checks that exact revision on Linux before publishing. The Linux
publish check builds default features, checks all targets with all features and the core, HTTP-only
and SSH-only library configurations, and runs all-feature library tests. A failed, cancelled or
missing check prevents publishing. Superseded Linux checks are cancelled; an active Publish job
finishes. Full integration, feature, macOS and Windows checks continue in main CI; publishing does
not wait for them. Repair a regression found after release and publish a patch release, or the
appropriate breaking version if its fix changes the public API.

Review the proposed version, changelog and supported scope before merging a release pull request.
The release workflow uses GitHub's default token, so checks on that pull request may require
approval or a close/reopen after its last update. The publish job uses a short-lived crates.io token
obtained through OIDC; do not add a persistent registry token. Publishing stays push-triggered:
crates.io rejects tokens from `workflow_run` events. Both the Linux check and publisher check out
`github.sha`, and only publishing receives `id-token: write`.

Rerun the push-triggered Release workflow after a transient Linux or publishing failure. Later main
pushes also retry unpublished versions, including a version bump already merged. Any unpublished
version committed to main can publish after its Linux check, even without a release pull request.
Manually dispatching Release only prepares a release pull request. The first release after changing
this workflow needs hosted verification of the exact-revision checkout and OIDC publishing path.

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
