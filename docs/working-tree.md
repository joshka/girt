# Working-Tree Status and Checkout

Use status to inspect staged and working changes, and checkout to materialize a tree. They run on
macOS/Linux; Windows status traversal and tree materialization are unsupported. Raw operations
compare literal bytes and POSIX modes. `Repository::status` applies index flags, `core.fileMode` and
ignore rules. `Repository::status_with_filters` also converts content through a `filter::Pipeline`;
`Repository::checkout_tree_with_filters` writes converted files.

## Inspect Raw Status

Run `cargo run --example status` for a disposable repository, or add a repository path to inspect an
existing one. `Repository::raw_status` compares the selected tree, index, and working files. It
separates staged changes, working-file changes, conflicts, untracked files, and unchecked gitlinks.
It reads content when cached index stat data differs or is racy, and does not refresh the index or
write files. Choose a baseline and untracked-file policy explicitly. The example uses `Head` and
`Repository::status` with `UntrackedFiles::Collapsed`, which lists untracked paths as `git status`
does after loading ignore rules with `Repository::ignore_rules`.

A status report is an observation, not an atomic snapshot or permission to overwrite a working tree.
The [status module contract](../src/status.rs) describes supported paths and modes. The
[compatibility record](compatibility.md#raw-working-tree-status) retains test scope and platform
evidence.

## Inspect Configured Status

Use `Repository::status` with rules from `Repository::ignore_rules` for repository policy. Open the
handle with `Repository::open_with_config` when the application needs system, global or runtime
configuration layers; `Repository::open` reads repository configuration only.

Intent-to-add entries contribute no staged content. Their existing working files appear as
`Change::Added`, including empty files; deleted files appear as `Change::Deleted`. Assume-unchanged
and skip-worktree entries suppress working differences but retain staged differences. These flags
are promises status trusts, so a clean report does not prove their content unchanged.
`core.fileMode=false` ignores executable-bit-only changes on regular files. Content changes and
changes between regular files and symlinks remain visible. Inspect `Report::comparison` when
consuming reports produced by different entry points. Use raw status when the application needs to
verify assume-unchanged files or compare literal modes.

## Check Out a Tree

Run `cargo run --example checkout` for a disposable create-and-remove lifecycle. The example owns
its repository exclusively and passes `None` as the initial clean baseline. For an existing index,
pass the tree represented by that index as the baseline. `Repository::checkout_tree` refuses staged
or unstaged changes, conflicts, and untracked obstructions before mutation. It materializes the
selected tree and publishes a matching index without switching HEAD or refs.

Exclude other worktree writers and metadata/path replacement for the whole call. Preparation checks
objects, names, obstructions, and the clean baseline while holding the index lock. A later failure
can leave applied file changes and a retained lock or temporary artifact. Inspect the returned
per-path report and actual files before recovery; retrying the original call without a new baseline
can be unsafe. The [checkout module contract](../src/checkout.rs) lists path, platform, and mutation
limits. The [compatibility record](compatibility.md#raw-tree-checkout) contains the tested phases
and failures.

For an end-to-end introduction to staging, raw status, ignores and checkout, see the
[working tree and index guide](../site/src/content/docs/guides/index-and-working-tree.md).
