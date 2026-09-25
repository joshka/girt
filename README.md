# girt

An incremental, idiomatic Rust implementation of Git, dual-licensed under MIT and Apache-2.0.

girt aims to provide a Git library for applications such as jj, with APIs organized around Git's own
concepts and exact interoperability for supported repository formats and operations.

## Status

The library derives SHA-1/SHA-256 object identities, encodes blobs, and reads and writes
SHA-1/SHA-256 loose blobs, trees, commits, and annotated tags in an explicitly selected object
directory. Reads validate object contents; writes publish complete objects without replacing
existing files. In-memory trees in both formats support byte-preserving names, standard entry modes,
payload parsing and encoding, Git ordering, and identity. Parsing preserves supported noncanonical
trees; construction validates names and duplicates. Commits preserve exact payloads and expose tree,
ordered parents, identities, dates, byte messages, and opaque multiline headers. Construction
validates fields separately from parsing existing commits. Annotated tags preserve target identity
and type, byte names, optional taggers, opaque extra headers, and message bytes, including embedded
signatures. Storing a tag object does not create a tag reference. Explicit-path repository opening
supports ordinary, bare, separate Git directories, and linked worktrees, deriving the object format
from repository-local configuration. Packs, refs, reflogs and working-tree index v2 support both
formats. Transport negotiation remains SHA-1-only. Unsupported configuration sources and repository
extensions return errors. Files-backend references support byte-preserving names, loose/packed
enumeration and reads, symbolic resolution, and conditional batches with explicit reflog policy,
plus single-ref operations without reflogs. HEAD and per-worktree refs use the detected layout.
Repository object reads combine live loose storage with bounded snapshots of SHA-1/SHA-256
pack/index v2 pairs, including OFS_DELTA and same-pack REF_DELTA reconstruction. The API is
experimental.

See the crate documentation (`cargo doc --open`) for runnable examples, API contracts, and
filesystem assumptions. [Compatibility evidence](docs/compatibility.md) records test provenance and
dependencies and [platform validation](docs/compatibility.md#platform-and-git-version-validation).
Reflog expiry, atomic multi-ref visibility, and a CLI are not implemented. Run
`cargo run --example reference_transaction` for conditional branch/tag publication with
caller-supplied reflog identity, timestamps and messages.

Repository discovery searches physical ancestors, with an optional inclusive ceiling. Initialization
creates ordinary or bare SHA-1/SHA-256 repositories with unborn `main` and refuses reinitialization.
See the [initialization example](examples/init_repository.rs) and
[discovery and initialization boundaries](docs/compatibility.md#repository-discovery-and-initialization).

`CloneRequest::prepare_tracking` prepares new bare repositories and ordinary repositories without
checkout through explicit local, HTTP or SSH endpoints. Both layouts retain remote-tracking
branches, tags and a selected local branch or detached HEAD, with persistent `origin` configuration.
See the [clone example](examples/clone_repository.rs) and
[scope and failure contracts](docs/compatibility.md#clone-without-checkout).

Commit ancestry is available through `Objects::walk`, `Objects::is_ancestor`, and
`Objects::merge_bases`; see the [history example](examples/history.rs) and
[completion evidence](docs/testing.md#commit-history-completion).

`Objects::compare_trees` reports recursive leaf additions, removals and changes with byte paths, IDs
and modes. Run `cargo run --example compare_trees` for a disposable comparison or pass a repository
and two tree IDs. See [tree comparison scope](docs/compatibility.md#tree-comparison).

`content_diff::diff` compares byte payloads with explicit binary policy and bounded shortest line
edits. `BlobContent::read` loads the blob sides of a tree change separately. Run
`cargo run --example content_diff` for a tree-to-content comparison; see
[content diff scope](docs/compatibility.md#content-diff).

SHA-1/SHA-256 working-tree index v2 entries can be parsed, constructed and encoded with
`index::Index`. `Repository::edit_index` holds `index.lock` from read through replacement, with
bounded input, byte paths, conflict stages and explicit extension restrictions. No working files are
created. Run `cargo run --example index`; see
[index scope](docs/compatibility.md#working-tree-index).

`Repository::raw_status` separates staged changes, literal working-file changes, conflicts and
unchecked gitlinks on macOS/Linux. It verifies content without refreshing the index and exposes
untracked/normalization policy explicitly. Run `cargo run --example status`; see
[raw status boundaries](docs/compatibility.md#raw-working-tree-status).

`Repository::checkout_tree` materializes a selected tree with raw bytes and POSIX modes on
macOS/Linux, then publishes its index without switching HEAD. It requires an explicit clean baseline
and refuses staged/unstaged changes and untracked obstructions. Run `cargo run --example checkout`;
see [checkout lifecycle and failure outcomes](docs/compatibility.md#raw-tree-checkout).

Pack/index v2 artifacts can be exported from an explicit object set with `write_pack`. Run
`cargo run --example write_pack` to export and reopen a private pack. The default writer streams
ordinary zlib entries. `write_pack_with_compression` enables bounded internal delta selection
through `PackCompression::Delta`; push exposes the same choice in `PushLimits::compression`.
Installation remains the caller’s responsibility. See
[delta compression](docs/compatibility.md#bounded-pack-delta-compression) for selection policy and
compatibility boundaries.

`fetch::receive` implements upload-pack protocol v0 over caller-supplied streams;
`fetch::receive_local` supplies a local Git upload-pack process adapter. Fetch validates complete
packs, deltas, identities, and selected-tip connectivity before explicit index-last installation.
Explicit `fetch::KnownHistory` enables bounded have/ACK negotiation and known-only no-op fetches;
installation rechecks the required local objects. `fetch::FetchRequest` composes refspec selection,
local/HTTP/SSH transfer, installation and conditional remote-tracking/tag publication, with explicit
force authorization and reflog policy. Run `cargo run --example fetch_remote` for this workflow or
`cargo run --example fetch_local` for the lower-level object-transfer APIs. Local branch
destinations and `FETCH_HEAD` are outside orchestration's supported scope. `remote::Remote` reads
named raw URLs and refspecs from the repository configuration snapshot; `remote::Refspecs` maps
explicit source refs without executing transfers or authorizing updates. Run
`cargo run --example remote_plan` for fetch selection and conditional push preparation. Credential
discovery, implicit remote/branch selection, shallow/partial fetches, automatic tags, and pruning
are not implemented. See
[fetch compatibility](docs/compatibility.md#upload-pack-fetch).

`push::PreparedPush` validates complete reachable histories and builds a non-thin pack.
`new_excluding` omits explicit receiver roots only when they belong to that validated graph and
remain advertised during sending. `push::send` publishes explicit conditional branch/tag commands
over receive-pack v0 streams; `push::send_local` supplies a local Git server adapter. Branch rewinds
and tag replacement require explicit force policy. Results preserve unpack and per-ref status,
including partial success; connection failures after transmission are distinguished as uncertain.
Local tracking refs remain unchanged. Run `cargo run --example push_local` for a disposable
branch/tag publication example. Deletion and atomic multi-ref push are deferred. See
[push compatibility](docs/compatibility.md#receive-pack-push).

Enable the `http` feature for async smart-HTTP(S) fetch and push using a caller-owned Tokio runtime.
`fetch::receive_http` downloads a response; its `DownloadedFetch::validate` step performs
synchronous pack validation before installation. `push::send_http` consumes an already prepared
push. Authentication headers and additional trust roots are explicit; redirects, proxies and
automatic retries are rejected or disabled. Run `cargo run --features http --example http_local` for
a disposable loopback example. See [HTTP setup and contracts](docs/http.md).

Enable `ssh` on macOS/Linux for async system OpenSSH fetch and push. Supply literal endpoint
components and an explicit executable/config file to `transport::ssh::SshRemote`.
`fetch::receive_ssh` returns an `DownloadedFetch` for separate validation; `push::send_ssh` borrows
a prepared push. Host-key checking and noninteractive policy are enforced. Run
`cargo run --features ssh --example ssh_local` against a disposable loopback sshd with temporary
keys and a restricted service command. See [SSH setup and contracts](docs/ssh.md).

Local adapters on macOS/Linux interrupt pipe and server-exit waits when cancelled. Their
`*_with_control` entry points also accept an absolute deadline; see
[transport interruption](docs/compatibility.md#owned-transport-interruption) for scope and cleanup.

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

## Optional Tracing

Enable `tracing` for operation and phase spans with categorical outcomes and work counts. The caller
owns the subscriber and logging policy. Run `cargo run --features tracing --example tracing`; see
[tracing contracts](docs/tracing.md) for coverage, safe fields, worker context and measured
overhead.
