---
title: Safety model
description: How girt avoids data loss, what it checks itself, and what the caller must guarantee.
---

:::note[Work in progress]
This page is an outline. The repository's rules for
[deleting and rewriting stored data](https://github.com/joshka/girt/blob/main/docs/storage-safety.md)
are the current source.
:::

Losing the last readable copy of an object can't be undone. girt treats data loss, corruption and
unsafe mutation as blockers, and refuses an operation when it can't meet its safety rules.

## Outline

- **Separate effects:** object installation, reference updates and working-tree changes are separate
  calls, so a failure leaves a state you can inspect.
- **Conditional publication:** reference transactions check expected old values, and what a lock
  does and doesn't protect against.
- **Publish, verify, retire:** replacements are written, synced and decoded before old data is
  removed.
- **Pinned snapshots:** scanning, selecting and copying through one open store, and detecting
  artifacts that change underneath an operation.
- **Caller-owned isolation:** which operations need you to exclude concurrent GC, pruning, or
  worktree and `HEAD` changes, and how
  [`MaintenanceIsolation`](https://docs.rs/girt/latest/girt/retention/trait.MaintenanceIsolation.html)
  expresses that.
- **Bounded resources:** limits on reads, history, packs and transfers.
- **Failure reports and recovery:** what a partial failure leaves behind and when it is safe to
  retry.
