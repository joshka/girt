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
not create a tag reference. Explicit-path repository opening supports ordinary, bare, separate Git
directories, and linked worktrees, deriving SHA-1 storage from repository-local configuration.
Unsupported configuration sources and repository extensions return errors. Files-backend references
support byte-preserving names, loose/packed reads, symbolic resolution, and conditional single-ref
updates explicitly without reflogs. HEAD and per-worktree refs use the detected layout. Repository
object reads combine live loose storage with bounded snapshots of SHA-1 pack/index v2 pairs,
including OFS_DELTA and same-pack REF_DELTA reconstruction. The API is experimental.

See the crate documentation (`cargo doc --open`) for runnable examples, API contracts, and
filesystem assumptions. [Compatibility evidence](docs/compatibility.md) records test provenance and
dependencies. SHA-256, reflogs, reference deletion, multi-ref transactions, upward repository
discovery, and a CLI are not implemented.

Commit ancestry is available through `Objects::walk`, `Objects::is_ancestor`, and
`Objects::merge_bases`; see the [history example](examples/history.rs) and
[completion evidence](docs/testing.md#commit-history-completion).

Pack/index v2 artifacts can be exported from an explicit object set with `write_pack`. Run
`cargo run --example write_pack` to export and reopen a private pack. The writer uses ordinary
zlib-compressed entries without delta selection; the writer leaves installation to callers.

`fetch::receive` implements upload-pack protocol v0 over caller-supplied streams;
`fetch::receive_local` supplies a local Git upload-pack process adapter. Fetch validates complete
packs, deltas, identities, and selected-tip connectivity before explicit index-last installation.
No-haves negotiation can retransmit history on incremental fetches. Reference updates remain
separate conditional operations without reflogs. Run `cargo run --example fetch_local` for a
complete disposable example. HTTP/SSH adapters, credentials, remote/refspec policy, shallow/partial
fetches, automatic tags, and pruning are not implemented. See
[fetch compatibility](docs/compatibility.md#upload-pack-fetch).

`push::PreparedPush` selects and validates complete reachable histories and builds a non-thin pack.
`push::send` publishes explicit conditional branch/tag commands over receive-pack v0 streams;
`push::send_local` supplies a local Git server adapter. Branch rewinds and tag replacement require
explicit force policy. Results preserve unpack and per-ref status, including partial success;
connection failures after transmission are distinguished as uncertain. Local tracking refs remain
unchanged. Run `cargo run --example push_local` for a disposable branch/tag publication example.
Deletion, atomic multi-ref push and HTTP/SSH adapters are deferred. See
[push compatibility](docs/compatibility.md#receive-pack-push).

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
annotated tag of a blob and read both objects back. Run
`cargo run --example open_repository -- /path/to/repo <blob-id>` to open an existing repository and
read a loose blob without modifying files. Run `cargo run --example publish_branch` to store commits
and publish/advance a branch through HEAD in a disposable repository, explicitly omitting reflogs.
Run `cargo run --example packed_repository -- /path/to/repo <object-id>` to print the exact payload
of a loose or packed object; its kind, identity, and size go to stderr.

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
