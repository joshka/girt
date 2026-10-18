# Working-Tree Status and Checkout

Use these operations when the application owns literal byte and POSIX-mode policy. They run on
macOS/Linux; Windows status traversal and tree materialization are unsupported. Raw status and
checkout apply no Git attributes, filters, EOL conversion, or configuration normalization;
`Repository::status_with_filters` compares working files after the conversions of a
`filter::Pipeline`, and `Repository::checkout_tree_with_filters` writes them converted. Only
`Repository::status` applies ignore rules, and only to untracked paths.

## Inspect Raw Status

Run `cargo run --example status` for a disposable repository, or add a repository path to inspect an
existing one. `Repository::raw_status` compares the selected tree, index, and working files. It
separates staged changes, working-file changes, conflicts, untracked files, and unchecked gitlinks.
It verifies content instead of trusting cached index stat data and does not refresh the index or
write files. Choose a baseline and untracked-file policy explicitly. The example uses `Head` and
`Repository::status` with `UntrackedFiles::Collapsed`, which lists untracked paths as `git status`
does after loading ignore rules with `Repository::ignore_rules`.

A status report is an observation, not an atomic snapshot or permission to overwrite a working tree.
The [status module contract](../src/status.rs) describes supported paths and modes. The
[compatibility record](compatibility.md#raw-working-tree-status) retains test scope and platform
evidence.

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
