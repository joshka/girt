# Save and restore changes with stashes

A stash saves the index and working files without moving HEAD. Use `stash::Snapshot` to read a
Git-created stash, `Objects::create_stash` to construct one from captured trees, and
`Objects::apply_stash` to merge it onto another index. `stash::History` lists and edits the
`refs/stash` reflog. Each step computes or publishes explicitly, so servers and jj can use the
object operations without a working tree.

## Capture and save snapshots

Capture three snapshots under your application's repository isolation: HEAD, the stage-zero index,
and tracked working files. Include staged additions/deletions in the tracked snapshot. Apply the
same clean-filter, symlink and file-mode policies used when staging those files. Unmerged indexes
must be resolved first. To include untracked files, capture them in a separate tree; include ignored
files only if the caller requested them. Install these input trees and blobs before constructing the
save plan.

Pass their identities in `SaveInput`, with explicit author and committer identities. `SaveOptions`
selects `All`, `KeepIndex` or `Staged`. Supply a short branch label (or `(no branch)`) and a
repository-unique abbreviation length for Git's default message. A custom message produces
`On branch: message`. The saved index and optional untracked commit have their own messages and
parent relationships; the worktree commit references them in Git's order.

`create_stash` returns `None` when the selected snapshots contain no changes. Otherwise:

1. Install `Save::commits()` in dependency order, after all captured input objects.
1. Observe `History`, then plan `History::push` with the saved ID, reflog identity and message.
1. Publish the log plan. Keep the saved objects even if publication fails.
1. Only after publication succeeds, remove the saved changes using `Save::cleanup` and restore
   `Save::index_after`. Revalidate captured paths before removing untracked files.

`KeepIndex` leaves the saved index in both the index and working files. `Staged` saves only staged
changes and computes their inverse against the working tree. If that cleanup conflicts, preserve the
saved entry and leave working files alone. Git's staged-only command can likewise save an entry and
then fail during patch-based cleanup. Girt returns original merge sides instead of claiming that the
provisional cleanup tree is resolved.

Filesystem capture and guarded dirty-worktree cleanup are application responsibilities;
[a repository workflow](https://github.com/joshka/girt/issues/639) tracks that higher-level API. The
generic tree checkout requires a clean index/worktree baseline and does not itself authorize
resetting a dirty capture. Do not bypass its refusal to implement save cleanup.

## Apply a Git-created stash

List saved entries with `History::entries()`; ordinal zero is the newest (`stash@{0}`). Load the
selected ID through `Snapshot::read`. A stash's first parent is its original HEAD, not the preceding
stash; the reflog provides the stack order.

Capture the current index tree and enumerate all existing untracked/ignored paths into
`ApplyInput::occupied`. Application uses the current index as ours, the saved base as ancestor, and
the saved working tree as theirs. Existing dirty tracked files are protected by guarded checkout;
they are not additional merge inputs. Saved untracked paths must not collide with any occupied file
or directory, even if their contents are identical.

Ordinary apply preserves existing staged changes and stages newly added paths. With
`ApplyOptions::restore_index`, girt first merges the saved index delta into the current index. If
that step conflicts, inspect `Apply::index_merge`; no working-tree merge is attempted and no
checkout plan is available. Git's patch-based index restoration can reject cases that tree merging
can resolve; applications requiring that stricter admission must impose it before publication.

For a working-tree conflict, `Apply::tracked` retains the original entries and content for rerere or
caller resolution. `Apply::prepare_checkout` prepares marker files and stages 1–3 through the
existing merge module. Clean preparation keeps the working snapshot and intended index distinct:
restoring a working edit does not accidentally stage it.

Under exclusive operation access, prepare and publish the result:

```rust
let applied = objects.apply_stash(&saved, input, options, &cancel)?;
if applied.index.is_some() {
    let checkout = applied.prepare_checkout(&objects, &markers)?;
    let report = repo.checkout_merge(Some(current_index_tree), &checkout)?;
    assert!(report.index_published);
}
```

Use the existing merge/checkout limits and marker policy. Checkout supports its existing Linux/macOS
filesystem policy; other platforms return a typed unsupported error. It revalidates the baseline and
protects untracked paths before working-file mutation. Preparation writes nothing; checkout installs
generated objects before writing files and publishes the final index under the same guard. A
checkout failure may report completed working-file changes. Preserve that report and the saved
entry; do not assume rollback or blindly retry.

## Pop and drop entries

Pop is successful apply followed by conditional drop. Observe history before applying, require
`Apply::is_clean()`, complete checkout/index publication, then publish `history.drop(ordinal)?`. A
conflict or partial checkout leaves the saved entry available. If another process changed the stash
history, the stale drop plan refuses; the working changes are already restored, so reconcile the
fresh log instead of applying them again.

`History::drop` removes an ordinal and rewrites predecessor IDs like Git. It does not delete any
objects. Dropping the final entry removes `refs/stash` and its log. List/drop/push publication is
currently limited to the files backend;
[reftable history editing](https://github.com/joshka/girt/issues/638) is an explicit refusal.

History observations are bounded and complete; exceeding the byte/entry limit is an error rather
than a truncated list. Publication locks the log, checks the original bytes and expected tip,
updates the tip conditionally, then replaces or removes the log. These two files are not a joint
crash transaction. `LogError::Transaction` retains reference-transaction receipts;
`LogError::Partial` names the published tip when later log publication failed. The original log
remains readable. Read actual refs/log bytes under isolation and reconcile them before another
operation; an inconsistent pair is deliberately refused by `History::read`.
