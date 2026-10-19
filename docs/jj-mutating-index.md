# jj mutating commands and the Git index

On the Linux v6.1 fixture (78,700 working-tree files, 5,088 trees), jj's mutating commands on girt
took about 0.3–0.45 s longer than stock jj ([#453](https://github.com/joshka/girt/issues/453)). The
`duplicate`, `bookmark create` and `commit` rows were 1.18–1.34 times stock on girt 0.17.0.

## Profile

`jj bookmark create` barely touches the working copy, so it shows the fixed cost of one transaction.
jj's own trace spans covered repository load and the working-copy snapshot, and these differed by
under 0.1 s. A sampled profile put most of the remaining time after the transaction:
`try_reset_git_head` → `reset_head` → `build_index_from_merged_tree`. After every transaction in a
colocated workspace, the adapter rebuilt the Git index by walking every tree of the working-copy
parent, about 5,000 tree reads. In that window, 1,008 of 1,448 main-thread samples were in this
path. Most of those were pack lookups and delta decoding inside girt's object reads. Stock jj does
the same full walk through gix's `index_from_tree`, but each read costs less there.

The [performance checklist](https://github.com/joshka/girt/issues/599) has no item for this exact
cost. The closest are the caching items (21). The fix follows the idea behind Git's cache-tree:
don't reread trees the index already describes.

## Adapter change

`reset_index` now opens the index first and calls `girt::index::Index::tree_id`. That hashes the
staged entries into a tree ID without reading any objects, and leaves intent-to-add entries out.

- If that tree is the resolved parent tree, the new entries are the existing staged entries, rebuilt
  fresh so stat reuse behaves as before. No trees are read.
- If it differs and is stored, as after `commit`, `new` or `edit`, the adapter diffs it against the
  parent tree. Only the trees that changed are read, and the entries are patched to match.
- Conflicted parent trees, unmerged, sparse or skip-worktree indexes, and index trees that are
  missing or unreadable fall back to the full walk.

The resulting entries match the full walk. A Linux check ran `commit` (with an added file, a removed
file, a new directory and a modified file), `new`, `bookmark create` and `edit` with both binaries.
`git ls-files -s` and `git status` output were identical after every step.

## Results

Wall-clock seconds, fresh `cp -c` copies per sample, four rounds alternating tool order (girt
0.17.0, patched adapter, stock; then the reverse). Load average about 11. Each tool's spread was
under 0.1 s, and the result held in both orders.

| Row               | girt 0.17.0 | patched | stock | patched / stock |
| ----------------- | ----------: | ------: | ----: | --------------: |
| `bookmark create` |        1.66 |    0.75 |  1.32 |            0.57 |
| `duplicate`       |        1.63 |    0.77 |  1.36 |            0.57 |
| `commit`          |        1.73 |    0.84 |  1.42 |            0.59 |

Medians. A medium spot check on the Git repository, two rounds, put the patched adapter at or below
stock on all three rows (0.11–0.21 s against 0.14–0.21 s).

Other #453 rows that end a transaction in a colocated workspace (`describe`, `new`, `squash`,
`edit`, `abandon`, `restore`, `undo`) go through the same `reset_head` path. They should gain
similarly, but they have not been re-measured here.

The change is on the fork's `girt-backend` branch as
[5a6b4b03](https://github.com/joshka/jj/commit/5a6b4b03612d39c247eb9106ae910cd16b4c8e45).
jj-lib's git tests (121) and jj-cli's colocated, init, private-commit and working-copy tests (72)
pass.
