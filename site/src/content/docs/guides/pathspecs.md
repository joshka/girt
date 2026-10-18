---
title: Limit operations to paths
description: Parse Git pathspecs once and use them to limit status, tree diffs and log walks.
---

Git commands take pathspecs to work on part of a repository: `git status -- src`,
`git diff A B -- '*.rs'`, `git log -- ':!vendor'`. girt parses them once into a
`girt::pathspec::Pathspec` and passes that value to each operation, so every caller gets the same
matching rules and the walks skip directories the pathspec cannot reach.

## Parse against the current directory

A pathspec is relative to where the user ran the command. Give `pathspec::Context` the current
directory as a repository-relative prefix, and read the `GIT_*_PATHSPECS` variables from the same
environment snapshot the rest of the program uses. `:/` and `:(top)` still start at the repository
root.

The one choice a caller has to make is what a list of only excludes starts from. `git status`,
`git diff` and `git log` start from the whole repository, so `:!vendor` run from `src` still reports
changes in `docs`. `git ls-files` and `git grep` start from the current directory. Set
`with_excludes_from_root(true)` for the first kind of command.

## Limit status and a tree diff

In a Cargo binary project, run `cargo add girt tempfile` and put this program in `src/main.rs`. It
builds two trees, compares them through a pathspec, and runs status from a subdirectory:

```rust
use std::sync::atomic::AtomicBool;

use girt::pathspec::{Context, Pathspec};
use girt::status::{Baseline, Limits, Untracked};
use girt::tree::TreeEditor;
use girt::{InitKind, ObjectFormat, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("w"), InitKind::Worktree)?;
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    editor.set(b"src/lib.rs", loose.write_blob(b"old\n")?)?;
    editor.set(b"src/gen/out.rs", loose.write_blob(b"old\n")?)?;
    editor.set(b"README", loose.write_blob(b"old\n")?)?;
    let old = editor.write()?;
    editor.set(b"src/lib.rs", loose.write_blob(b"new\n")?)?;
    editor.set(b"src/gen/out.rs", loose.write_blob(b"new\n")?)?;
    editor.set(b"README", loose.write_blob(b"new\n")?)?;
    let new = editor.write()?;

    // As if the user ran `girt diff A B -- '*.rs' ':!gen'` from `src`.
    let context = Context::new()
        .with_prefix(b"src")
        .with_excludes_from_root(true);
    let paths = Pathspec::parse(["*.rs", ":!gen"], &context)?;
    let cancel = AtomicBool::new(false);
    let objects = repo.objects()?;
    let changes =
        objects.compare_trees_matching(Some(old), Some(new), &paths, Default::default(), &cancel)?;
    let changed: Vec<_> = changes.iter().map(|change| change.path.as_slice()).collect();
    assert_eq!(changed, [&b"src/lib.rs"[..]]);

    // The same value limits status. Nothing is staged yet, so only the untracked file shows.
    let worktree = repo.worktree().expect("worktree repository");
    std::fs::create_dir_all(worktree.join("src"))?;
    std::fs::write(worktree.join("src/main.rs"), b"fn main() {}\n")?;
    std::fs::write(worktree.join("notes.txt"), b"scratch\n")?;
    let report = repo.raw_status_with(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        &paths,
        Limits::default(),
        &cancel,
    )?;
    assert_eq!(report.raw_untracked, [b"src/main.rs".to_vec()]);
    Ok(())
}
```

`src/gen` is never entered by either operation: `Pathspec::directory` reports it as `Skip`, so the
tree comparison does not read its tree and status does not list it.

## Walk a path-limited history

`git log -- <pathspec>` lists the commits that change a selected path. For each commit the walk
reaches, call `Objects::simplify_commit` with the commit, its parents and the pathspec. Show the
commit when `shown` is true and continue through `follow` only. A merge that left the selected paths
as one parent had them is hidden, and the walk follows only that parent, so side branches that never
touched those paths are skipped. `girt log` combines this with a committer-date queue to print the
same commits, in the same order, as `git log`.

## What is not supported

Status, tree comparison and history refuse `attr:` magic with an unsupported error, because they do
not load attributes for every path; `Pathspec::matches_with` supports it for callers that do.
Absolute paths are rejected even inside the worktree. History offers only Git's default
simplification: there is no `--full-history`, `--simplify-merges`, parent rewriting or `--follow`.
