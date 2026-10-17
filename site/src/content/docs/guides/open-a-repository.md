---
title: Open a repository
description: Choose a repository explicitly or discover one above a directory.
---

Use an explicit path when a project picker, command argument, or saved workspace already identifies
a repository. Use discovery when a user selects a directory inside a checkout. Both operations read
metadata. Neither scans all objects or changes files.

For trusted local use, start with [`Repo`](https://docs.rs/girt/latest/girt/struct.Repo.html).
`Repo::discover` loads Git-default configuration and opens object storage lazily with default
limits. Use [`Repository`](https://docs.rs/girt/latest/girt/struct.Repository.html) for explicit
control over configuration, limits, cancellation, and untrusted input. `Repo::repository()` exposes
that API when one operation needs more control.

## Create a disposable project

Add the dependencies with `cargo add girt tempfile`. This complete program creates a checkout, opens
it again, and finds it from a nested directory:

```rust
use girt::{InitKind, ObjectFormat, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("project");
    Repository::init(ObjectFormat::Sha1, &path, InitKind::Worktree)?;
    let repository = Repository::open(&path)?;
    let nested = path.join("src");
    std::fs::create_dir(&nested)?;
    let discovered = Repository::discover_with_ceiling(&nested, &path)?;
    assert_eq!(discovered.git_dir(), repository.git_dir());
    assert_eq!(repository.object_format(), ObjectFormat::Sha1);
    Ok(())
}
```

Run it with `cargo run`. The temporary directory removes the repository when the program exits. For
a persistent project, pass its real path to `Repository::open` and skip initialization:
`Repository::init` refuses a directory that already has metadata, so it can't open or repair a
repository.

## Choose a discovery boundary

A tool that works on one selected project should pass that project's directory as the ceiling; the
ceiling itself is included in the search. Discovery resolves symlinks and then searches physical
ancestors. If it finds a candidate that is malformed or unsupported, it stops there instead of
opening an enclosing repository. Metadata can still point outside the ceiling, so the ceiling
doesn't isolate filesystem access or tell you whether to trust the repository.

If discovery reports `NotFound`, check that the start directory exists and the ceiling includes the
repository you meant. If it reports malformed metadata, inspect the reported path instead of
searching past it. If it reports an unsupported repository, girt can't work with that repository;
don't retry as if it used default settings. See
[opening errors](https://docs.rs/girt/latest/girt/repository/enum.OpenError.html).

## Keep metadata and storage separate

Use `git_dir()`, `common_dir()`, and `object_dir()` instead of constructing `.git` paths. Linked
worktrees have private metadata and shared storage; bare repositories have no working directory.
Opening reads configuration and shallow metadata once, so reopen the repository when you need fresh
metadata. When reading many objects, keep one object reader so pack indexes aren't opened again for
each read.

Opening does not read process-global Git configuration or environment. Applications that need those
sources supply them through
[ConfigInputs](https://docs.rs/girt/latest/girt/config/struct.ConfigInputs.html). Applications that
must decide whether to trust a repository before reading its configuration can first select an exact
[RepositoryLocation](https://docs.rs/girt/latest/girt/repository/struct.RepositoryLocation.html).

Continue with the [objects and history guide](/girt/guides/objects-and-history/) to build and
inspect a snapshot. The
[Repository API](https://docs.rs/girt/latest/girt/repository/struct.Repository.html) documents
layout, resource, and filesystem contracts.
