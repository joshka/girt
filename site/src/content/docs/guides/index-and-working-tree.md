---
title: Working tree and index
description: Stage explicit bytes, inspect raw status, apply ignore rules, and check out a tree.
---

The [working-tree contract](https://github.com/joshka/girt/blob/main/docs/working-tree.md) records
the raw status and checkout guarantees.

The index records the file identities and modes proposed for the next tree. The working tree holds
editable files. HEAD supplies the usual comparison baseline. These are three independent states:
writing a file does not stage it, and changing the index does not write a working file or commit.

Use girt's raw operations when your application decides how bytes are normalized and how the
filesystem is treated. They preserve literal content and POSIX modes, and they do not apply
attributes, clean/smudge filters, EOL conversion, ignore rules, or configuration overrides for
executable bits and symlinks; [Line endings and filters](/girt/guides/filters/) covers the
converting variants. Status and checkout support Linux and macOS; macOS requires ASCII paths.
Windows traversal is unsupported.

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
one normal entry. `intent_to_add`, `skip_worktree` and `assume_valid` are preserved as data; girt
doesn't decide from them which paths are staged or checked out sparsely.

Reading a split index resolves its immutable shared file. An edit that changes entries publishes a
standalone index, and `make_standalone` removes the dependency without changing entries. Sparse
expansion reads collapsed directory trees into entries without writing files; it doesn't choose
which paths are checked out. Unknown mandatory extensions are refused, and unknown optional
extensions prevent editing until the caller chooses to discard them.

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

`Repository::edit_index` reads that same logical snapshot and publishes a standalone replacement
when entries change. `IndexEdit::make_standalone` also allows conversion without changing entries.
It removes derived caches but retains resolve-undo information. Conversion never deletes the shared
file: another index may still depend on it. Publication checks that both original files still match
the snapshot, so a changed shared file can reject a commit even though the replacement no longer
needs it.

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
`edit_index_at_with_options` only when you need those layouts. Split dependencies resolve beside the
selected lexical path. Following a leaf symlink reads its referent, but commit replaces the symlink
itself; it does not write through it. Keep this in mind when a caller supplies the index path.

### Expand sparse entries without changing the checkout

A sparse index collapses a skipped directory into a tree entry. `Index::expand_sparse` reads that
tree and produces its leaf entries in memory. It does not create files, publish an index, or clear
skip-worktree flags, so raw status and checkout still refuse unsupported flags after expansion. Your
application has to decide which paths take part before it uses those operations. See the runnable
[sparse expansion example](https://docs.rs/girt/latest/girt/index/struct.Index.html#method.expand_sparse).

Expansion has independent traversal and output budgets. Cancellation, a missing object, a wrong
object kind, or a limit failure leaves the original collapsed draft intact. Diagnose an object read
failure through its source chain, because raising a budget cannot restore a missing tree.

### Decide which extension information may be lost

Optional extensions can carry information an edit would invalidate, and being optional in the format
does not make them safe to discard. A rejected edit leaves the guard and its draft available; choose
a supported operation or abort instead of retrying with an empty index.

- Entry-offset accelerators (`EOIE`, `IEOT`) can be removed together with `invalidate_entry_offsets`
  along with fsmonitor (`FSMN`) and untracked (`UNTR`) caches. Tree-cache invalidation, resolve-undo
  removal, entry replacement and framing changes also discard derived caches.
- Resolve-undo (`REUC`) remembers prior conflict stages. `discard_resolve_undo` intentionally
  forgets that information; call it only when that matches the application operation.
- The cached tree (`TREE`) can be invalidated without changing staged entries.
- Unknown optional extensions stay until you choose to discard them. Unknown mandatory extensions
  are refused; choosing to discard doesn't apply to them.

Checkout drops derived `TREE`, `FSMN`, `UNTR`, `EOIE` and `IEOT` caches, even when the target is
unchanged. Git rebuilds these caches when enabled. Checkout still refuses split/sparse indexes,
resolve-undo data and unknown extensions. Status scans working files without trusting these caches.

Each operation accepts a different set of extensions. Check each method's errors before combining
them; removing one cache successfully does not mean every later rewrite is allowed.

### Reuse stat caches without trusting them as content

`replace_entries_reusing_stat` preserves old stat words only for entries with the same path, stage,
mode, identity and flags. Use it when you rebuild the whole entry list and want to keep the cached
stat data. Changed entries retain the supplied stat words. The operation never checks the working
file, and cached equality is not proof that its content is unchanged. Commit clears cached size for
entries modified at or after the earlier of the previous index modification time and the current
write time. Stat-cache consumers must then verify content, even when rewriting the index would
otherwise hide a racily clean entry.

The index version doesn't affect which entries are staged: v4 compresses repeated path prefixes,
while v2 is useful for older consumers. Extended flags require v3 or v4; requesting v2 does not
remove them. Paths remain bytes, including non-UTF-8 names, across supported codec round trips. The
repository's object format selects the checksum width; it must be known before parsing an index.

## Read the two status comparisons

`Repository::raw_status` compares baseline-to-index for staged changes and index-to-working-files
for unstaged changes. Choose `Baseline::Head` to resolve this worktree's HEAD, or `Baseline::Tree`
for an explicit tree; `None` means empty. An unborn HEAD also means an empty baseline.

`Report` keeps staged changes, unstaged changes, unresolved entries, unchecked gitlinks, raw
untracked leaves and nested-repository boundaries separate. An empty unstaged list does not
establish that submodules are clean. As in Git, a file whose stat data matches the index and was not
modified around the time the index was written is not read; assume-valid is ignored. Status never
refreshes or writes the index, runs hooks, stages files, or detects renames.

`Untracked::Omit` avoids unrelated traversal. `RawFilesWithoutIgnores` lists raw leaves without
reading their contents, including ignored names; empty directories are omitted. To hide ignored
files as Git does, call `Repository::status` instead (see below). Use the executable
[raw status example](https://docs.rs/girt/latest/girt/status/) for a staged file that is modified in
the working tree.

A status report describes the files as they were while it ran. It is not an atomic snapshot and does
not make it safe to overwrite files. A `Changed` error means a file, a directory, the index or
`HEAD` changed while status ran; retrying can refresh a display. Mutation needs exclusive ownership
even if a preceding status succeeded. Limits bound reads, visited names, paths and content bytes.
Cancellation is cooperative; these synchronous APIs belong on the application's blocking worker when
used from an async runtime.

### Record fresh stat data explicitly

Because status never writes, a file whose stat data went stale while its content did not (after
`touch`, a build tool, a branch switch, or a girt checkout, which records no stat data) is read
again by every status call. `Repository::refresh_index` is the explicit write, like
`git update-index --refresh`. It holds `index.lock` for the whole scan, compares files as
`Repository::status` does, and records stat data only for files whose content it read and found
equal to the index. Pass a filter pipeline to `refresh_index_with` to compare converted content, as
`status_with_filters` does. A file modified in the second the refresh began is recorded with a zero
size, so the next status still reads it. When nothing needs recording, nothing is written. A held
lock is reported as `StorageError::Locked` rather than waited for or skipped.

### Interpret every status category

A path with unresolved stages appears in `unmerged`, not in either change list. Present the
base/ours/theirs entries that exist and preserve their IDs until the application resolves the
conflict by replacing them with stage zero through
[index editing](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.edit_index)
and
[entry replacement](https://docs.rs/girt/latest/girt/index/struct.IndexEdit.html#method.replace_entries).
An empty change list does not resolve a conflict.

An unstaged `Deleted` means the exact tracked name or an ancestor is absent. `Obstructed` means
another filesystem kind or a repository boundary blocks that path. Report the obstruction's path
rather than offering to discard it. `Modified` carries the observed identity and mode, so a
mode-only change can be explained without inventing a content edit.

`unchecked_gitlinks` contains every stage-zero gitlink, without inspecting whether a directory
exists. Girt does not open its commit target or check whether the submodule is dirty, so a UI that
needs that information must inspect the submodule itself. A nested repository boundary means
traversal stopped there; its contents are not untracked leaves.

Choose budgets for the whole observation: a single-file ceiling does not bound the sum of thousands
of small files. `Untracked::Omit` reduces unrelated traversal, but tracked-path verification still
reads content. A limit failure returns no partial report. Preserve the last successful display with
a visible failure state instead of replacing it with a clean result.

## List untracked files as Git does

`Repository::ignore_rules` loads the ignore files Git reads: the `core.excludesFile` file (or its
`$XDG_CONFIG_HOME/git/ignore` default) and `info/exclude`, with `core.ignoreCase` choosing the
comparison. Open the repository with `ConfigInputs::git_default` so the user's global configuration
is visible. `Repository::status` then compares tracked content exactly as `raw_status` does and
fills `Report::untracked` with the untracked paths that no rule excludes, reading each visited
directory's `.gitignore` as it goes.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::status::{Baseline, UntrackedFiles, UntrackedKind};
    use girt::workflow::Environment;
    use girt::{InitKind, ObjectFormat, Repository};

    let directory = tempfile::tempdir()?;
    let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("w"), InitKind::Worktree)?;
    let worktree = repo.worktree().unwrap();
    std::fs::create_dir_all(worktree.join("target/debug"))?;
    std::fs::write(worktree.join("target/debug/app"), b"binary")?;
    std::fs::create_dir(worktree.join("notes"))?;
    std::fs::write(worktree.join("notes/todo.md"), b"todo")?;
    std::fs::write(worktree.join(".gitignore"), b"/target/\n")?;

    // An isolated environment, so no personal excludes file applies.
    let rules = repo.ignore_rules(&Environment::default())?;
    let report = repo.status(Baseline::Head, UntrackedFiles::Collapsed, &rules)?;
    let listed: Vec<_> = report.untracked.iter().map(|entry| (&entry.path[..], entry.kind)).collect();
    assert_eq!(
        listed,
        [(&b".gitignore"[..], UntrackedKind::File), (b"notes", UntrackedKind::Directory)]
    );
    Ok(())
}
```

`UntrackedFiles::Collapsed` lists a directory without tracked paths once, as `git status` does by
default; `All` lists every file, like `--untracked-files=all`. Tracked paths are never ignored, and
an ignored directory such as `target/` is not entered at all, so build output costs nothing.
`WorktreeRules::check` answers single-path questions the way `git check-ignore -v` does, and
`WorktreeRules::source` names the file and line behind a decision.

## Apply ignore rules with provenance

`Ignore` is a pure byte-path matcher; `WorktreeRules` is built on it. The caller loads the
configured global excludes file, common-directory `info/exclude` and encountered `.gitignore` files;
it must not follow `.gitignore` symlinks. The matcher reads no filesystem, configuration,
environment or index. The caller decides how ignore results affect untracked files; they don't apply
to tracked paths automatically.

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
paths and pass the `InputKind` you observed; a symlink is a file, never a directory to traverse.
Backslash remains a literal byte, including on Windows. A path or work-limit error supplies no
partial decision; do not silently translate it into included or excluded.

### Match patterns as byte paths

A slashless pattern such as `*.log` matches a basename at any depth below its source. A leading
slash anchors it to the source directory, and an embedded slash matches a relative path there. A
trailing slash requires a directory; symlinks are queried as files. `*` and `?` stay within a path
component, while supported `**` forms can cross directory boundaries. Character classes match bytes,
not Unicode characters.

A leading `#` starts a comment; escape it to match a literal leading hash. A leading `!` negates a
rule; escape it for a literal exclamation mark. Unescaped trailing spaces are trimmed. CRLF and an
initial UTF-8 BOM are recognized. Invalid glob syntax remains a rule that never matches, so adding
it successfully is not evidence that it will select any path.

Choose `Case::Sensitive` or `Case::AsciiInsensitive` explicitly. ASCII folding also affects
directory source prefixes; neither policy performs Unicode normalization or detects filesystem
behavior. `check` includes excluded-parent decisions. `check_direct` is useful while traversing only
when the caller has already admitted every ancestor. It can return an including leaf negation even
when a parent is excluded. Use `check` when the caller has not enforced parent decisions.

Source addition is atomic on a budget failure. Existing rules remain usable, but the rejected source
has contributed no rules. Query work exhaustion produces no decision. Retain source labels indexed
by successful additions so `Match.source` and its physical line number can explain the result.

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
