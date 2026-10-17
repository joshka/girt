---
title: Getting started
description: Add girt to a Rust project, open a repository and read its history.
---

Add girt to a project, open an existing repository and print its recent history. Nothing here writes
to disk; the guides linked at the end cover writing objects, updating references and transfer.

## Requirements

- Rust 1.97.1 or newer.
- macOS, Linux or Windows for storage, references and local transfer. Raw status and checkout are
  supported only on macOS and Linux.

## Add the dependency

```sh
cargo add girt
cargo add clap --features derive
```

The example below uses [clap](https://docs.rs/clap) for its arguments; girt doesn't need it.

The default build has no network transport. Enable the optional features you need:

| Feature   | Adds                                                     |
| --------- | -------------------------------------------------------- |
| `http`    | Smart HTTP(S) fetch and push, on a Tokio runtime you own |
| `ssh`     | Fetch and push through system OpenSSH (macOS and Linux)  |
| `tracing` | Operation spans, without installing a subscriber         |

## Read a repository

This program prints the first ten commits in `HEAD`'s history, one line each:

```rust
use std::path::PathBuf;

use clap::Parser;
use girt::{HistoryLimits, PackLimits, ReadLimits, Repository};

/// Print the first commits in HEAD's history.
#[derive(Parser)]
struct Args {
    /// Repository or worktree to read.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Number of commits to print.
    #[arg(short = 'n', long, default_value_t = 10)]
    count: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repository = Repository::open(&args.path)?;
    let head = repository.head()?;
    let head_id = head.id.ok_or("HEAD has no commits yet")?;
    let objects = repository.objects()?;
    let history = objects.walk(&[head_id])?;
    for id in history.into_iter().take(args.count) {
        let commit = objects.read_commit(id)?;
        let message = String::from_utf8_lossy(commit.message());
        let summary = message.lines().next().unwrap_or_default();
        println!("{id} {summary}");
    }
    Ok(())
}
```

Run it against a repository or a linked worktree:

```sh
cargo run -- /path/to/repo
```

Each step is one call:

1. [`Repository::open`] reads the repository's configuration and layout. It doesn't search parent
   directories, so pass the repository or worktree itself.
2. [`Repository::head`] follows `HEAD` to the branch it names, or stops at `HEAD` if it's detached,
   and returns the commit ID stored there. A new repository's branch has no commits yet (Git calls
   this an _unborn_ branch), so the ID is an `Option`. The program reports that case as an error
   rather than printing nothing.
3. [`Repository::objects`] opens a reader over the repository's loose objects and packs.
4. [`Objects::walk`] collects every commit reachable from `HEAD` by following parent links, before
   the program prints the first few. The order is breadth-first from `HEAD`, not sorted by date.
   `HistoryLimits::default()` refuses a history over 100,000 commits; raise `max_commits` for a
   larger repository.
5. [`Objects::read_commit`] reads one commit and parses it.

## Habits of the API

The program shows a few patterns that run through girt.

**Budgets are explicit.** `PackLimits`, `ReadLimits` and `HistoryLimits` cap the input an operation
accepts, such as object size, delta depth and the number of commits walked. Input over budget is
refused with an error. The defaults suit ordinary repositories; raise them only for repositories you
trust with more work. The budgets don't cap total memory use.

**Absence is a value, damage is an error.** An unborn branch is `id: None`, and [`Objects::read`]
returns `None` for an object that doesn't exist. Errors report I/O failures and corrupt, unsupported
or over-budget data. Typed reads such as [`Objects::read_commit`] treat a missing object as an
error, because an ID you got from a reference or a walk should exist.

**A reader is a snapshot of packs.** An [`Objects`] reader keeps the packs it opened even if another
process repacks, while loose objects are read from disk at each call. Open a new reader to see new
packs.

[`Repository::open`]: https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.open
[`Repository::head`]: https://docs.rs/girt/latest/girt/repository/struct.Repository.html
[`Repository::objects`]: https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.objects
[`Objects::walk`]: https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.walk
[`Objects::read`]: https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.read
[`Objects::read_commit`]: https://docs.rs/girt/latest/girt/objects/struct.Objects.html
[`Objects`]: https://docs.rs/girt/latest/girt/objects/struct.Objects.html

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
