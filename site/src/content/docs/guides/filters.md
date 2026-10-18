---
title: Line endings and filters
description: Convert files between the repository and the working tree as Git does, and decide whether filter programs such as Git LFS may run.
---

Git does not always store a file's bytes as they are in the working tree. `.gitattributes` and
configuration ask for conversions: line endings (`text`, `eol`, `core.autocrlf`), `$Id$` keywords
(`ident`), UTF-16 files stored as UTF-8 (`working-tree-encoding`), and filter programs
(`filter=<name>`), which is how Git LFS stores a small pointer in the repository and the real file
in the working tree. A status or checkout that ignores them reports false changes or writes the
wrong files.

girt makes these conversions with a `filter::Pipeline`. The core `Repository` APIs take one
explicitly; the `Repo` convenience handle builds one for you.

## Repo runs filter programs

`Repo::clone`, `Repo::status` and `Repo::switch` convert files as `git` does, and that includes
**running filter programs**: the `filter.<name>.clean`, `smudge` and `process` commands configured
in local, global or system configuration, chosen for each path by the repository's `.gitattributes`.
Configuration is never copied by a clone, but the attributes are part of the repository you check
out, so a repository can pick which of your configured filters run on its files.

To convert line endings, `ident` and encodings without running any program, opt out. A path whose
filter would run a program then fails with an error instead. A clone through `workflow::clone_with`
takes the same choice in `CloneOptions::with_filter_commands`, where `None` writes raw bytes:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::filter::Commands;
    use girt::{InitKind, ObjectFormat, Repo, Repository};

    if cfg!(not(any(target_os = "linux", target_os = "macos"))) {
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("w"),
        InitKind::Worktree,
    )?;
    // `Repo::discover` reads configuration as `git` does; the opt-out is the same there.
    let repo = Repo::new(repository).with_filter_commands(Commands::Refuse);
    let report = repo.status()?;
    assert!(report.unstaged.is_empty());
    Ok(())
}
```

## Choose explicitly with Repository

`Repository::filters` loads the pipeline Git would use: attribute files, `core.autocrlf`,
`core.eol`, `core.safecrlf` and the `filter.<name>.*` drivers. Its `Commands` argument is the trust
decision. `Commands::Refuse` never starts a process; `Commands::Run` runs configured commands
through `sh -c` in the working tree root, and keeps a long-running `process` filter alive for the
pipeline's lifetime.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;

    use girt::filter::{Commands, Staged};
    use girt::index::{Entry, Mode};
    use girt::status::{Baseline, Limits, UntrackedFiles};
    use girt::workflow::Environment;
    use girt::{InitKind, ObjectFormat, Repository};

    if cfg!(not(any(target_os = "linux", target_os = "macos"))) {
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("w"),
        InitKind::Worktree,
    )?;
    let worktree = repository.worktree().unwrap().to_path_buf();
    std::fs::write(worktree.join(".gitattributes"), "*.bat text eol=crlf\n")?;
    let environment = Environment::default().with("GIT_ATTR_NOSYSTEM", "1");
    let mut filters = repository.filters(&environment, Commands::Refuse)?;

    // What `git add` would store for one file: CRLF becomes LF.
    let file = b"echo\r\n";
    let stored = filters.to_repository(b"run.bat", file, Staged::Absent)?;
    assert_eq!(&*stored.bytes, b"echo\n");

    // Stage the stored form; the CRLF working file then compares clean.
    std::fs::write(worktree.join("run.bat"), file)?;
    let id = repository.loose_objects().write_blob(&stored.bytes)?;
    let mut edit = repository.edit_index(Default::default())?;
    edit.replace_entries(vec![Entry::new(b"run.bat".to_vec(), Mode::Regular, id)])?;
    edit.commit()?;
    let rules = repository.ignore_rules(&environment)?;
    let report = repository.status_with_filters(
        Baseline::Head,
        UntrackedFiles::Omit,
        &rules,
        &girt::pathspec::Pathspec::all(),
        &mut filters,
        Limits::default(),
        &AtomicBool::new(false),
    )?;
    assert!(report.unstaged.is_empty());
    Ok(())
}
```

`Repository::checkout_tree_with_filters` takes two pipelines. The baseline's files are checked
against the attributes now in the working tree (`Repository::filters`), and the target is written
with its own (`Repository::filters_for_checkout`), because the target's `.gitattributes` files are
not on disk yet. As in Git, `attr.tree` or `GIT_ATTR_SOURCE` overrides both.

## What matches Git, and what doesn't

Conversions are compared byte for byte with `git add`, `git checkout-index`, `git status` and
`git checkout`; the
[compatibility record](https://github.com/joshka/girt/blob/main/docs/compatibility.md#content-filters)
lists the cases. Two Git behaviors are worth knowing:

- A file whose size differs from the size cached in the index is reported modified, even when its
  converted content is unchanged. Changing only a file's line endings shows up in status although a
  diff would be empty. girt reports it the same way.
- A failing filter that is not `filter.<name>.required` leaves content unconverted; Git prints an
  error and continues. girt reports it in `Converted::skipped` and, for checkout,
  `checkout::Report::unfiltered`.

Not supported yet: the process protocol's `delay` capability, and `working-tree-encoding` values
outside UTF-16 and UTF-32 ([issue 593](https://github.com/joshka/girt/issues/593)).
