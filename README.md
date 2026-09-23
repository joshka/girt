# girt

An incremental, idiomatic Rust implementation of Git, dual-licensed under MIT and Apache-2.0.

girt aims to provide a Git library for applications such as jj, with APIs organized around Git's own
concepts and exact interoperability for supported repository formats and operations.

## Status

The library derives SHA-1 blob identities, encodes blobs, and reads and writes loose blobs, trees,
commits, and annotated tags in an explicitly selected object directory. Reads validate object
contents; writes publish complete objects without replacing existing files. In-memory SHA-1 trees
support byte-preserving names, standard entry modes, payload parsing and encoding, Git ordering, and
identity. Parsing preserves supported noncanonical trees; construction validates names and
duplicates. Commits preserve exact payloads and expose tree, ordered parents, identities, dates,
byte messages, and opaque multiline headers. Construction validates fields separately from parsing
existing commits. Annotated tags preserve target identity and type, byte names, optional taggers,
opaque extra headers, and message bytes, including embedded signatures. Storing a tag object does
not create a tag reference. The API is experimental.

See the crate documentation (`cargo doc --open`) for runnable examples, API contracts, and
filesystem assumptions. [Compatibility evidence](docs/compatibility.md) records test provenance and
dependencies. SHA-256, history traversal, refs, packs, repository discovery, and a CLI are not
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

Run `cargo run --example loose_blob` for a complete write/read operation in a disposable object
store. The temporary directory is removed when the example exits normally. Run
`cargo run --example tree` to build, encode, and parse an in-memory tree. Run
`cargo run --example loose_tree` to write blobs, construct a tree referencing them, store it, and
read the tree and its blobs back. Run `cargo run --example loose_commit` to store a blob, its tree,
and a root commit, then read the snapshot back. Run `cargo run --example loose_tag` to store an
annotated tag of a blob and read both objects back.

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
