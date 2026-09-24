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
Clippy use your default Rust toolchain.

## Development Checks

For Markdown-only changes, run `just fmt-md-check` (rumdl). Use `just fmt-md` to fix formatting. For
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
[Blob Performance Baseline](docs/benchmarks.md) for workloads, measured operations, cache
conditions, and recorded results.

The [platform workflow](.github/workflows/validation.yml) repeats runtime and interoperability
checks on macOS and Linux, with a separate Windows portable-test job. See
[platform evidence and gaps](docs/compatibility.md#platform-and-git-version-validation) before
making support claims. Local cross-compilation does not replace executing the test suite on the
target OS.

## Implementation and Tests

Use [Testing and Completion Criteria](docs/testing.md) to define acceptance evidence for each
capability before implementation. Include its concrete checklist in the task description. Unit
tests, integration tests, and benchmarks should address the change's contracts and risks.

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

Contributions must be original and suitable for distribution under `MIT OR Apache-2.0`. Do not copy,
translate, or adapt copyrightable expression from Git source code, including its tests and comments.
Non-copyrightable ideas, algorithms, and processes may inform independent implementations. Record
specification references and fixture provenance, and check dependency licenses before adding them.

## Pull Requests

Explain the problem, resulting behavior, validation performed, and remaining limitations. Link
relevant issues and call out public API changes, new dependencies, and compatibility assumptions.
Keep documentation consistent with what the library actually implements.

## HTTP Fixtures

The optional `http` feature uses a caller-owned Tokio runtime. Its Unix interoperability tests need
Git, Python 3 and OpenSSL on PATH and use only disposable loopback servers. `just check` enables
this feature; also run `cargo check --no-default-features` when changing feature gates. See
[HTTP setup and contracts](docs/http.md) for the example, TLS fixtures and async processing
boundary.

## SSH Fixtures

The optional `ssh` feature uses system OpenSSH and a caller-owned Tokio runtime on macOS/Linux. Its
loopback interoperability tests require Python 3, Git, ssh, ssh-keygen and an unprivileged sshd that
can authenticate the current OS user with a temporary key and a forced command. No persistent
SSH/account configuration is changed. Missing or restricted sshd is a concrete fixture failure, not
a skipped compatibility result. See [SSH contracts and setup](docs/ssh.md).
