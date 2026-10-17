---
title: Overview
description: What girt is, what it is for, and where it stands.
---

girt is a Rust library for applications that work with Git repositories. It opens a repository at a
path you choose and works with its Git data directly: objects, trees, commits, tags, references,
reflogs, the index and the working tree. Transfer is explicit. You fetch, clone and push over local
paths, smart HTTP(S) or SSH, and your application decides when downloaded objects become reachable
through a reference update.

girt is dual-licensed under MIT and Apache-2.0. It is an original implementation. Compatibility
comes from Git's format documentation and from observing the `git` executable, not from Git, gix or
libgit2 source.

## Who it's for

girt is for Rust programs that need Git storage and transfer as a library: version-control tools,
forges, build systems and other programs that would otherwise shell out to `git`. Its first consumer
is [jj](https://github.com/jj-vcs/jj). An experimental jj branch replaces gix and the Git subprocess
with girt and passes jj's test suite. See [girt as jj's Git backend](/girt/jj/backend/).

girt has no command-line interface, and it doesn't aim to reproduce `git`'s porcelain behaviour.

## What it supports

- **Repositories:** SHA-1 and SHA-256, ordinary and bare, linked worktrees, discovery, layered
  configuration, alternates and shallow history.
- **Objects:** blobs, trees, commits and tags in loose and packed storage, pack reading and writing,
  delta compression, history walks, merge bases, tree comparison and content diffs.
- **References:** the files backend (loose and packed) and reftable, conditional transactions,
  reflogs and tag peeling.
- **Working tree:** index v2, v3 and v4, raw status and tree checkout on macOS and Linux.
- **Transfer:** fetch, clone and push over local paths without running Git, over smart HTTP(S)
  (`http` feature) and through system OpenSSH (`ssh` feature).
- **Maintenance:** retention planning, additive repacking, reflog expiry, pack retirement and loose
  object pruning.

[Supported and refused](/girt/compatibility/supported-and-refused/) lists what each area supports
and what it rejects.

## Where it stands

girt is experimental and its API still changes between minor releases. It runs jj's full test suite
and an end-to-end script against real Git. Performance is the main open work. Medium-sized
repositories are mostly at parity with stock jj, while some operations on very large repositories
are still several times slower. The [benchmark](/girt/performance/jj-benchmark/) shows where.

## How this site relates to the API docs

The [API documentation on docs.rs](https://docs.rs/girt/latest/girt/) is self-contained. Its crate
and module pages explain each area, and every item documents its contract, limits and how to recover
from failure. This site covers tasks and concepts that span several modules, the safety and
compatibility model, and performance results. It links to docs.rs for details instead of repeating
them.

## Next steps

- [Getting started](/girt/getting-started/): add girt to a project and read a repository.
- [Design principles](/girt/concepts/design-principles/): why girt's API has the shape it has.
- [The Git model](/girt/concepts/git-model/): the Git concepts girt exposes and how they map to
  types.
