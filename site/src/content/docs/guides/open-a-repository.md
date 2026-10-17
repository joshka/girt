---
title: Open a repository
description: Choose a repository explicitly or discover one above a directory.
---

Use an explicit path when a project picker, command argument, or saved workspace already identifies
a repository. Use discovery when a user selects a directory inside a checkout. Both operations read
metadata; neither scans all objects or changes files.

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
a persistent project, pass its real path to `Repository::open` and omit initialization.
Initialization refuses existing metadata; it is not a way to open or repair a repository.

## Choose a discovery boundary

A tool operating on one selected project should normally set that project's directory as the
inclusive ceiling. Discovery searches physical ancestors after resolving symlinks. It stops at a
recognized malformed or unsupported candidate instead of opening an enclosing repository. Metadata
indirections can still point outside the ceiling, so this boundary does not isolate filesystem
access or establish trust.

If discovery reports `NotFound`, check that the start exists and the ceiling includes the intended
repository. If it reports malformed metadata, inspect the reported path rather than searching past
it. An unsupported extension requires a capability decision, not a retry with default settings. See
[opening errors](https://docs.rs/girt/latest/girt/enum.OpenError.html).

## Keep metadata and storage separate

Use `git_dir()`, `common_dir()`, and `object_dir()` instead of constructing `.git` paths. Linked
worktrees have private metadata and shared storage; bare repositories have no working directory.
Opening captures configuration and shallow metadata. Reopen when you need fresh metadata, and retain
an object reader while reading many objects to avoid repeatedly opening pack indexes.

Opening does not read process-global Git configuration or environment. Applications needing those
sources supply them through
[ConfigInputs](https://docs.rs/girt/latest/girt/config/struct.ConfigInputs.html). Applications that
must apply trust policy before reading configuration can first select an exact
[RepositoryLocation](https://docs.rs/girt/latest/girt/struct.RepositoryLocation.html).

Continue with the [objects and history guide](/girt/guides/objects-and-history/) to build and
inspect a snapshot. The [Repository API](https://docs.rs/girt/latest/girt/struct.Repository.html)
documents layout, resource, and filesystem contracts.
