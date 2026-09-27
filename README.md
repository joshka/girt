# girt

girt is an experimental Rust library for applications that work with Git repositories, including jj.
It is dual-licensed under MIT and Apache-2.0.

## Status

The current library can open SHA-1 repositories and work with their objects, history, and
references. It also exposes local fetch and push through Git processes. The API may change; there is
no CLI.

Callers choose repository paths and handle reference updates separately from fetches. A push can
partially succeed; inspect its per-ref results before retrying. SHA-256 repositories are not
supported. The [crate Rustdoc source](src/lib.rs) gives API contracts and filesystem assumptions;
[compatibility evidence](docs/compatibility.md) records exact boundaries and
[platform validation](docs/compatibility.md#platform-and-git-version-validation).

## Start Here

Run `cargo run --example loose_blob` to write and read a blob in a disposable object store. The
temporary directory is removed when the example exits normally. For API contracts and runnable Rust
examples, build the crate documentation with `cargo doc --open`.

Choose another example for the operation you need:

- `tree` builds and parses an in-memory tree; `loose_tree`, `loose_commit`, and `loose_tag` store
  and read object graphs in disposable storage.
- `open_repository -- /path/to/repo <blob-id>` reads a loose blob from an existing repository;
  `packed_repository -- /path/to/repo <object-id>` reads loose or packed object payloads. These
  examples do not modify the supplied repository.
- `publish_branch` stores commits and advances a branch through HEAD without reflogs;
  `history -- /path/to/repo <commit-id> [other-commit-id]` walks commit ancestry; `write_pack`
  exports and reopens a private pack.
- `fetch_local` and `push_local` demonstrate transport through disposable local Git processes.

Prefix each name with `cargo run --example` to run it. The commands that take a repository path
require the additional arguments shown.

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
