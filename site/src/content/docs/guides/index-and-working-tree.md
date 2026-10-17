---
title: Working tree and index
description: Stage explicit bytes, inspect raw status, apply ignore rules, and check out a tree.
---

The [working-tree contract](https://github.com/joshka/girt/blob/main/docs/working-tree.md) records
the raw status and checkout guarantees.

The index records the file identities and modes proposed for the next tree. The working tree holds
editable files. HEAD supplies the usual comparison baseline. These are three independent states:
writing a file does not stage it, and changing the index does not write a working file or commit.

Use girt's raw operations when your application owns byte normalization and filesystem policy. They
preserve literal content and POSIX modes. They do not apply attributes, clean/smudge filters, EOL
conversion, ignore rules, or configuration overrides for executable bits and symlinks. Status and
checkout support Linux and macOS; macOS requires ASCII paths. Windows traversal is unsupported.

## Stage bytes deliberately

Write the blob first, then select its identity in a locked index edit. Derive the replacement from
that edit's snapshot so unrelated staged paths survive. `replace_entries` replaces the complete
list, so omitting a path unstages/removes that entry. It does not delete the corresponding working
file.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::index::{Entry, Mode};
    use girt::{InitKind, ObjectFormat, Repository};

    let directory = tempfile::tempdir()?;
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        InitKind::Worktree,
    )?;
    let id = repository.loose_objects().write_blob(b"hello\n")?;
    let mut edit = repository.edit_index(Default::default())?;
    let mut entries = edit.index().entries().to_vec();
    entries.retain(|entry| entry.path != b"hello.txt");
    entries.push(Entry::new(b"hello.txt".to_vec(), Mode::Regular, id));
    edit.replace_entries(entries)?;
    edit.commit()?;
    assert_eq!(
        repository
            .read_index(Default::default())?
            .unwrap()
            .entries()[0]
            .id,
        id
    );
    Ok(())
}
```

This disposable example needs `girt` and `tempfile`. No working file is created. An absent index is
`None` on read; committing an empty edit creates an existing, empty index. Treating a failed index
read as empty can discard staged work.

An entry has a byte path, mode, identity, conflict stage, flags and cached stat words. The codec
validates structure and stage relationships, not object existence or checkout safety. Stage zero is
a resolved selection. Stages one, two and three describe the base, ours and theirs of an unresolved
conflict; a missing side represents an absent file. Resolve a conflict by replacing its stages with
one normal entry. `intent_to_add`, `skip_worktree` and `assume_valid` preserve data without
supplying staging or sparse-checkout policy.

Split-index reads resolve their immutable shared file. Edits normally publish a standalone index;
`make_standalone` explicitly removes the dependency without changing entries. Sparse expansion reads
collapsed directory trees into entries without materializing files. It is separate from selecting a
sparse-checkout policy. Unknown mandatory extensions are refused; unknown optional extensions can
prevent editing until the caller explicitly chooses to discard them.

Batch paths in one index edit: opening reads the complete index and committing encodes the whole
file. One edit per path repeatedly pays that cost.

Index edits use `index.lock` and publish by same-directory rename. Existing locks are never stolen.
Drop abandons an edit with best-effort cleanup; `abort` reports cleanup errors. Optional file
syncing does not sync the parent directory and does not establish power-loss durability.

## Preserve index information when editing

A split index stores most entries in an immutable `sharedindex.<id>` file beside the primary index.
`Repository::read_index` resolves the pair into logical entries; the primary bytes alone are
insufficient. Keep both files when diagnosing a failed read. A missing shared file is an error, even
if the primary index is present and has a valid checksum.

`Repository::edit_index` reads that same logical snapshot and normally publishes a standalone
replacement when entries change. `IndexEdit::make_standalone` also allows conversion without
changing entries. It removes derived caches but retains resolve-undo information. Conversion never
deletes the shared file: another index may still depend on it. Publication checks that both original
files still match the snapshot, so a changed shared file can reject a commit even though the
replacement no longer needs it.

Choose an explicit path for an alternate index, such as a temporary staging view. These methods do
not consult `GIT_INDEX_FILE`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::index::{Entry, Mode, Version};
    use girt::{InitKind, ObjectFormat, Repository};

    let directory = tempfile::tempdir()?;
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        InitKind::Bare,
    )?;
    let path = directory.path().join("preview.index");
    let blob = repository.loose_objects().write_blob(b"preview\n")?;
    let mut edit = repository.edit_index_at(&path, Default::default())?;
    edit.replace_entries(vec![Entry::new(b"preview.txt".to_vec(), Mode::Regular, blob)])?;
    edit.set_version(Version::V4)?;
    edit.commit()?;
    let preview = repository.read_index_at(&path, Default::default())?.unwrap();
    assert_eq!(preview.entries()[0].id, blob);
    assert!(repository.read_index(Default::default())?.is_none());
    Ok(())
}
```

The basic alternate-index methods refuse split dependencies and leaf symlinks. Use
`edit_index_at_with_options` only when you deliberately need those layouts. Split dependencies
resolve beside the selected lexical path. Following a leaf symlink reads its referent, but commit
replaces the symlink itself; it does not write through it. This distinction matters for an index
selected by a caller-provided path.

### Sparse entries are a representation, not a checkout policy

A sparse index collapses a skipped directory into a tree entry. `Index::expand_sparse` reads that
tree and produces its leaf entries in memory. It does not create files, publish an index, or clear
skip-worktree flags. Raw status and checkout still refuse unsupported flags after expansion. An
application needs an explicit policy for which paths participate before using those operations. See
the runnable
[sparse expansion example](https://docs.rs/girt/latest/girt/index/struct.Index.html#method.expand_sparse).

Expansion has independent traversal and output budgets. Cancellation, a missing object, a wrong
object kind, or a limit failure leaves the original collapsed draft intact. Diagnose an object read
failure through its source chain; raising a budget cannot restore a missing tree.

### Decide which extension information may be lost

Optional extensions can carry information an edit would invalidate. Their optional format status
does not make discarding them harmless. A rejected edit leaves the guard and its draft available;
choose a supported operation or abort rather than silently retrying with an empty index.

- Entry-offset accelerators (`EOIE`, `IEOT`) can be removed together with `invalidate_entry_offsets`
  before an edit that changes entry framing.
- Resolve-undo (`REUC`) remembers prior conflict stages. `discard_resolve_undo` intentionally
  forgets that information; call it only when that matches the application operation.
- The cached tree (`TREE`) can be invalidated without changing staged entries.
- Unknown optional extensions require an explicit discard decision. Unknown mandatory extensions
  cannot be admitted by that decision.

These operations have different extension admission rules. Consult each method's errors before
combining them; one successful cache removal does not authorize every subsequent rewrite.

### Reuse stat caches without trusting them as content

`replace_entries_reusing_stat` preserves old stat words only for entries with the same path, stage,
mode, identity and flags. It helps applications that reconstruct a whole entry list avoid throwing
away useful cache data. Changed entries retain the supplied stat words. The operation never checks
the working file, and cached equality is not proof that its content is unchanged. Commit also
handles racily clean timestamps by clearing cached size where necessary.

Framing is independent of content selection: v4 compresses repeated path prefixes, while v2 is
useful for older consumers. Extended flags require v3 or v4; requesting v2 does not remove them.
Paths remain bytes, including non-UTF-8 names, across supported codec round trips. The repository's
object format selects the checksum width; it must be known before parsing an index.

## Read the two status comparisons

`Repository::raw_status` compares baseline-to-index for staged changes and index-to-working-files
for unstaged changes. Choose `Baseline::Head` to resolve this worktree's HEAD, or `Baseline::Tree`
for an explicit tree; `None` means empty. An unborn HEAD also means an empty baseline.

`Report` keeps staged changes, unstaged changes, unresolved entries, unchecked gitlinks, raw
untracked leaves and nested-repository boundaries separate. An empty unstaged list does not
establish that submodules are clean. Neither stat-cache equality nor assume-valid bypasses content
verification. Status never refreshes or writes the index, runs hooks, stages files, or detects
renames.

`Untracked::Omit` avoids unrelated traversal. `RawFilesWithoutIgnores` lists raw leaves without
reading their contents, including ignored names; empty directories are omitted. Apply ignore policy
explicitly if the UI needs it. Use the executable
[raw status example](https://docs.rs/girt/latest/girt/status/) for a staged file that is modified in
the working tree.

A status report is an observation, not an atomic snapshot or permission to overwrite files. A
`Changed` error reports an observed race; retrying can help refresh a display. Mutation needs
exclusive ownership even if a preceding status succeeded. Limits bound reads, visited names, paths
and content bytes. Cancellation is cooperative; these synchronous APIs belong on the application's
blocking worker when used from an async runtime.

## Apply ignore rules with provenance

`Ignore` is a pure byte-path matcher. The caller loads the configured global excludes file,
common-directory `info/exclude` and encountered `.gitignore` files; it must not follow `.gitignore`
symlinks. The matcher reads no filesystem, configuration, environment or index. Ignore decisions
apply to the caller's untracked-file policy, not automatically to tracked paths.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::ignore::{Case, Ignore, InputKind, Limits, Source};

    let mut rules = Ignore::new(Case::Sensitive, Limits::default());
    rules.add(Source::Global, b"*.log\n")?;
    let local = rules.add(Source::Directory(b""), b"!keep.log\nbuild/\n!build/keep\n")?;
    assert!(!rules.check(b"keep.log", InputKind::File)?.unwrap().ignored);
    let decision = rules.check(b"build/keep", InputKind::File)?.unwrap();
    assert!(decision.ignored);
    assert_eq!((decision.source, decision.line), (local, 2));
    Ok(())
}
```

Directory rules override global/info rules; deeper directory sources override shallower ones. At the
same level, later rules win. Explicit command rules have the highest precedence. An excluded parent
prevents re-inclusion of its child: the decision above comes from `build/`, not `!build/keep`.

`None` means no rule matched; `Some` with `ignored == false` identifies an explicit negation. The
source index and one-based line support UI explanations. Use slash-separated repository-relative
paths and supply the observed `InputKind`; a symlink is a file, never a directory to traverse.
Backslash remains a literal byte, including on Windows. A path or work-limit error supplies no
partial decision; do not silently translate it into included or excluded.

## Materialize a tree conservatively

`Repository::checkout_tree` writes a selected tree and matching index without switching HEAD or
refs. Supply the tree represented by the clean current index as the baseline, or `None` for an empty
index such as a no-checkout clone. A nonempty baseline with a missing index is refused. See the
runnable
[checkout example](https://docs.rs/girt/latest/girt/checkout/).

Hold exclusive control of worktree writers, root/ancestor renames and metadata replacement for the
whole call. The index lock coordinates cooperating index writers; it does not lock working files.
Preparation verifies trees, blobs, the clean baseline, names and obstructions before mutation.
Checkout refuses dirty tracked paths, conflicts, untracked obstructions, gitlinks, nested
repositories, unsupported extended flags and unsafe/aliased paths. It retains unrelated content and
empty directories. Files receive permissions 0644 or 0755; ownership, ACLs and other metadata are
not preserved.

Checkout has no automatic rollback or batch atomicity. A late failure can leave changed working
files and the old index. Inspect `Failure.report.applied`, the stage, `index_published`, the cause
and every cleanup failure. Reconcile actual files under exclusive ownership before choosing a new
baseline; blindly retrying the original call can be unsafe. Cancellation follows the same rule.

## API reference

- [Index codec and editing](https://docs.rs/girt/latest/girt/index/)
- [Raw status and errors](https://docs.rs/girt/latest/girt/status/)
- [Checkout reports and recovery](https://docs.rs/girt/latest/girt/checkout/)
- [Ignore matching and budgets](https://docs.rs/girt/latest/girt/ignore/)
