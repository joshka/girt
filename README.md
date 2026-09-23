# girt

An incremental, idiomatic Rust implementation of Git, dual-licensed under MIT and Apache-2.0.

girt aims to provide a Git library for applications such as jj, with APIs organized around Git's own
concepts and exact interoperability for supported repository formats and operations.

## Status

The library derives SHA-1 blob identities, encodes blobs, and reads and writes loose blobs in an
explicitly selected object directory. Reads validate object contents; writes publish complete
objects without replacing existing files. The API is experimental.

See the crate documentation (`cargo doc --open`) for runnable examples, API contracts, and
filesystem assumptions. [Compatibility evidence](docs/compatibility.md) records test provenance and
dependencies. SHA-256, other object types, packs, repository discovery, and a CLI are not
implemented.

## Design Goals

- Model objects, trees, commits, references, the index, and repositories directly.
- Preserve Git's formats and semantics for each supported feature.
- Offer idiomatic Rust APIs without reproducing another library's API or Git's CLI behavior.
- Build narrow, usable increments, with explicit errors for unsupported cases.
- Keep operations understandable locally and introduce abstractions only when they reduce
  complexity.

Compatibility will be established through format specifications, independently generated fixtures,
and tests against Git's observable behavior. All implementation, documentation, and tests must be
original; do not copy, translate, or adapt copyrightable expression from Git source code.

## Development

The crate uses Rust 2024. Build and run the current tests with:

```sh
cargo build
cargo test
```

Formatting uses nightly rustfmt and rumdl. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup,
formatting commands, compatibility testing expectations, and contribution guidance.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.
