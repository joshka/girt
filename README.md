# girt

girt is an experimental Rust library for applications that work with Git repositories, including jj.
It is dual-licensed under MIT and Apache-2.0.

## Status

girt can open SHA-1 and SHA-256 repositories and work with their objects, references, and indexes.
It also provides history queries and explicit local, HTTP, and SSH transfer workflows. The API may
change; there is no CLI.

Raw status and checkout run on macOS/Linux and do not apply Git's attribute, filter, or EOL
transformations. Native local transfer requires callers to exclude external GC/pruning and
worktree/HEAD changes through publication. Inspect partial publication before retrying. The
[crate Rustdoc source](src/lib.rs) documents API contracts;
[compatibility evidence](docs/compatibility.md) records tested boundaries and provenance.

## First use

Add `girt = "0.1"` to your dependencies (Rust 1.97.1 or newer). This small example computes a Git
blob identity without opening a repository:

```rust
use girt::{ObjectFormat, ObjectKind};

let id = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"hello");
assert_eq!(id.to_string(), "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0");
```

Run a blob write and read in a disposable object directory:

```sh
cargo run --example loose_blob
```

The example prints the object ID and removes its temporary directory when it exits. To read an
existing repository without changing it, run
`cargo run --example open_repository -- /path/to/repo <blob-id>`. Run `cargo doc --open` for the
crate documentation on object identities, repository reads, and public API contracts. Its
[Rustdoc source](src/lib.rs) is available in this repository.

## Choose a workflow

The [examples](examples/) show library calls for individual tasks. Check each example's setup before
running it; the linked guides describe prerequisites, effects, limits, and recovery.

- **Create or inspect a repository:** Run `cargo run --example init_repository`, then read
  [repository layouts and shallow history](docs/repositories.md).
- **Write and read objects:** Run `cargo run --example loose_commit`, then use
  `cargo run --example packed_repository -- /path/to/repo <object-id>` to inspect an existing
  repository. See the [crate Rustdoc source](src/lib.rs) for object and pack APIs.
- **Edit refs with explicit history:** Run `cargo run --example reference_transaction` and read
  [conditional references and reflogs](docs/reference-transactions.md), including partial failure
  recovery.
- **Resolve or edit configuration:** Run `cargo run --example config` or
  `cargo run --example edit_config`; see [layered configuration](docs/configuration.md).
- **Fetch, clone, or push:** Start with `fetch_remote`, `clone_repository`, or `push_local` in
  [examples/](examples/); see
  [transfer scope and evidence](docs/compatibility.md#upload-pack-fetch).
- **Inspect and update a working tree:** Run `cargo run --example status` or
  `cargo run --example checkout`; read the [working-tree guide](docs/working-tree.md) for
  preconditions and failure recovery.

For HTTP, enable `http` and start with `cargo run --features http --example http_local`; see
[HTTP setup and contracts](docs/http.md). For SSH on macOS/Linux, enable `ssh` and start with
`cargo run --features ssh --example ssh_local`; its loopback fixture needs sshd and temporary keys.
See [SSH setup and contracts](docs/ssh.md). The `tracing` feature and its
[operation guide](docs/tracing.md) are optional.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, compatibility testing, and contribution
guidance. The [roadmap](docs/jj-roadmap.md) tracks integration and remaining work.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.
