# Contributing to girt

girt is being built incrementally as an idiomatic Rust Git library. Keep contributions focused on
one useful capability and explain its supported behavior and limitations. Discuss substantial API or
architecture changes before implementing them.

## Setup

Install a stable Rust toolchain supporting edition 2024, plus the formatting tools:

```sh
rustup toolchain install nightly --component rustfmt
cargo install just --locked
cargo install rumdl --locked
```

Nightly is needed for rustfmt's unstable options. Normal builds and tests use your default Rust
toolchain.

## Development Checks

Run these checks before submitting a contribution:

```sh
just fmt
just fmt-check
cargo test
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

Use `just fmt-rust` or `just fmt-md` to format one language, and `just fmt-rust-check` or
`just fmt-md-check` to check it without changes. Markdown prose wraps at 100 characters; table
columns and separators stay aligned, even when a table must be wider.

## Implementation and Tests

Organize modules under `src/` around coherent Git concepts. Prefer readable control flow, explicit
side effects, and types that protect meaningful invariants. Document public API contracts and add
Rustdoc examples when an API becomes usable.

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
