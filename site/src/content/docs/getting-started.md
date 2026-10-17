---
title: Getting started
description: Add girt to a Rust project, open a repository and read its history.
---

Add girt to a project, open an existing repository, resolve `HEAD` and walk its history. Nothing
here writes to disk; the guides linked at the end cover writing objects, updating references and
transfer.

## Requirements

- Rust 1.97.1 or newer.
- macOS, Linux or Windows for storage, references and local transfer. Raw status and checkout are
  supported only on macOS and Linux.

## Add the dependency

```sh
cargo add girt
```

The default build has no network transport. Enable the optional features you need:

| Feature   | Adds                                                     |
| --------- | -------------------------------------------------------- |
| `http`    | Smart HTTP(S) fetch and push, on a Tokio runtime you own |
| `ssh`     | Fetch and push through system OpenSSH (macOS and Linux)  |
| `tracing` | Operation spans, without installing a subscriber         |

## Read a repository

This program prints the commit `HEAD` points to and counts the commits reachable from it. It follows
at most five symbolic references from `HEAD`. An ordinary `HEAD` needs one hop, to its branch; the
limit stops a reference cycle or an unexpectedly long chain.

```rust
use girt::refs::RefName;
use girt::{Commit, HistoryLimits, ObjectKind, PackLimits, ReadLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: PATH")?;
    let repository = Repository::open(path)?;

    // Follow HEAD through at most five symbolic hops to a commit ID.
    let head = repository
        .references()?
        .resolve(&RefName::new("HEAD")?, 5)?;
    let Some(head_id) = head.id else {
        let name = String::from_utf8_lossy(head.name.as_bytes());
        println!("{name} is unborn");
        return Ok(());
    };

    // An Objects reader pins the packs it opened; loose objects are read live.
    let objects = repository.objects(PackLimits::default())?;
    let object = objects
        .read(head_id, ReadLimits::default())?
        .ok_or("HEAD commit is missing")?;
    let kind = object.kind();
    if kind != ObjectKind::Commit {
        return Err(format!("HEAD names a {kind:?}, not a commit").into());
    }
    let commit = Commit::parse(repository.object_format(), object.data())?;
    let message = String::from_utf8_lossy(commit.message());
    println!("{head_id} {}", message.lines().next().unwrap_or_default());

    // Walk ancestry without relying on commit timestamps.
    let count = objects.walk(&[head_id], HistoryLimits::default())?.len();
    println!("{count} commits reachable from HEAD");
    Ok(())
}
```

Run it against any repository, including a linked worktree:

```sh
cargo run -- /path/to/repo
```

The example shows four habits that run through girt's API:

- **Budgets are explicit.** `PackLimits`, `ReadLimits` and `HistoryLimits` set budgets for the
  inputs an operation accepts, such as object size, delta depth and the number of commits walked.
  Input over budget is refused with an error. The budgets don't cap total memory use. The defaults
  suit ordinary repositories; raise them only for repositories you trust with more work.
- **Reading is separate from parsing.** [`Objects::read`] checks the object's framing and identity
  and returns its kind and exact bytes. Check the kind, then decode the bytes with [`Commit::parse`]
  for the repository's object format.
- **Packs are a snapshot; loose objects are live.** An [`Objects`] reader keeps the packs it opened
  even if another process repacks. Loose objects are read from disk at each call. Open a new reader
  to see new packs.
- **Missing isn't an error.** A missing object is `None`, and an unborn `HEAD` has no ID. Errors
  report I/O failures and corrupt, unsupported or over-budget data.

[`Objects::read`]: https://docs.rs/girt/latest/girt/struct.Objects.html#method.read
[`Commit::parse`]: https://docs.rs/girt/latest/girt/struct.Commit.html#method.parse
[`Objects`]: https://docs.rs/girt/latest/girt/struct.Objects.html

## Run the examples

girt's [examples](https://github.com/joshka/girt/tree/main/examples) each run a complete workflow.
Most create a disposable repository, so you can run them from a source checkout:

```sh
git clone https://github.com/joshka/girt
cd girt
cargo run --example init_repository
cargo run --example loose_commit
cargo run --example reference_transaction
cargo run --example history -- /path/to/repo <commit-id>
```

Check what an example writes before pointing it at a repository you care about.

## Where to go next

- Read the [guides](/girt/guides/open-a-repository/) for writing objects, updating references,
  working trees and transfer.
- Read the [safety model](/girt/compatibility/safety-model/) before using girt to mutate
  repositories that other processes also use.
- Browse the [crate documentation](https://docs.rs/girt/latest/girt/) for the full API map.
