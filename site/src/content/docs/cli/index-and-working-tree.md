---
title: Index and working tree
description: Inspect tracked state, ignore decisions and checkout effects.
---

`index list` reads tracked paths, `status` reports staged and unstaged changes (`--untracked` also
lists untracked files), and `ignore check` explains ignore rules. `checkout` replaces the index and
working tree without moving HEAD. `worktree list`, `lock`, `unlock`, `prune` and `repair` manage
linked-worktree registrations. Run `girt help checkout` and `girt help worktree` before changing
state.

## List tracked paths

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

```text
$ girt index list
notes.txt
$ git ls-files
notes.txt
```

The index records `notes.txt`. `girt --git status --untracked` corresponds to
`git status --porcelain`; without `--untracked`, use `git status --porcelain -uno`. Compare
`girt --git ignore check` to `git check-ignore -v`, and `girt --git worktree list` to
`git worktree list`. Checkout's equivalent is `git read-tree -um <tree>` rather than `git checkout`:
it does not switch branches. Use a disposable copy for checkout experiments.

## Library API and limits

Index listing reads [index::Index](https://docs.rs/girt/latest/girt/index/struct.Index.html). Status
and ignore use [Repository](https://docs.rs/girt/latest/girt/repository/struct.Repository.html)
methods `ignore_rules` and `status`; checkout calls `checkout_tree_with_progress`. Registration
commands use `worktrees`, `lock_worktree`, `unlock_worktree`, `prune_worktree` and `repair_worktree`
on that same type.

Status and checkout compare raw bytes on macOS/Linux, without filters, EOL conversion or
`core.filemode`; macOS paths must be ASCII. Checkout drops derived fsmonitor and untracked caches,
and refuses split/sparse indexes, resolve-undo data and unknown extensions. Prune deletes a missing
checkout's registration, HEAD and index; inspect its help before use. See
[The index and working tree](/girt/guides/index-and-working-tree/) for safety and partial-failure
contracts.
