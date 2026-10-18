---
title: Maintenance and garbage collection
description: Plan retention, choose cutoffs, and reclaim storage without losing readable objects.
---

Maintenance removes stored data that nothing still needs and keeps every object that something does.
Walking from refs alone misses data that is still in use:
[staged blobs](/girt/guides/index-and-working-tree/), reflog history, linked worktrees, recent
writes, application-owned heads and alternate-store relationships can all keep data alive. Girt
can't discover every external reader or writer, so your application chooses the retention policy and
supplies the isolation.

The [storage-safety model](https://github.com/joshka/girt/blob/main/docs/storage-safety.md) explains
the rules behind these APIs. The main one is that deleting the last readable copy can't be undone,
so if maintenance can't observe the repository completely or can't establish exclusion, it refuses
the operation.

## Choose an operation

| Task                    | API                       | What changes                                      |
| ----------------------- | ------------------------- | ------------------------------------------------- |
| Explain retention       | `plan_retention`          | Read-only plan and typed outcome                  |
| Add packed storage      | `repack_retained`         | New/reused indexed pack; old storage remains      |
| Expire history          | `expire_reflogs`          | Eligible reflog records, with durable replacement |
| Reclaim old loose files | `prune_unreachable_loose` | Eligible unreachable primary loose files          |
| Replace old packs       | `retire_old_packs`        | Verified replacement, then eligible old artifacts |
| Expire and prune        | `collect_garbage`         | Reflogs, pack consolidation if due, loose pruning |
| Run all phases          | `run_maintenance`         | Expiry, pack retirement, then loose pruning       |

`collect_garbage` consolidates packs only when there are more of them than Git's `gc.autoPackLimit`
allows (50 by default; 0 turns it off), counting indexed packs without a `.keep` file as
`git gc --auto` does. Select `PackConsolidation::Always` or `PackConsolidation::Never` with
`RetentionPolicy::with_pack_consolidation` to override the configuration, and use
`collect_garbage_with` to bound the consolidated pack. Consolidation publishes and verifies one pack
before retiring the ones it replaces, including recent packs, whose objects it keeps. `.keep` and
promisor packs stay in place. No routine operation such as fetch or commit ever consolidates.

The standalone prune and retirement operations retain all observed reflog history, even if the
policy contains expiry cutoffs. The composed operations expire reflogs first, update their
observation to match the rewritten reflogs, and then reclaim data, all under one isolation guard. A
successful additive repack doesn't make its inputs safe to delete.

## Understand roots and plan outcomes

`RetentionPolicy` defaults to retaining every reflog record, loose object and pack regardless of
age, so nothing is reclaimed until you change the policy. Supply `heads` for objects your
application owns but Git refs do not name, such as unpublished operation state.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;

    use girt::retention::{RetentionOutcome, RetentionPolicy};
    use girt::{InitKind, ObjectFormat, Repository};

    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo.git"),
        InitKind::Bare,
    )?;

    let id = repo.loose_objects().write_blob(b"application-owned")?;
    let policy = RetentionPolicy::default().with_heads(vec![id]);
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    match plan.outcome {
        RetentionOutcome::Complete => assert!(plan.required.contains(&id)),
        RetentionOutcome::Incomplete(error) => return Err(error.into()),
        _ => return Err("unknown retention outcome".into()),
    }

    drop(plan);
    Ok(())
}
```

This example needs `girt` and `tempfile`; it changes only a disposable repository. Returning the
typed `RetentionError` inside the boxed error preserves its source chain for application
diagnostics.

Planning includes live refs, independently discovered reflog endpoints, object-bearing pseudorefs,
main and registered linked-worktree HEAD/index data, caller heads and recent loose roots. A missing
checkout does not erase valid registered private metadata. Shallow commits stop parent traversal;
gitlinks are external and do not pull submodule objects into this repository.

A `RetentionPlan` preserves `roots`, `strong_roots`, `reachable`, `required`, expiry candidates and
protected storage. `reachable` includes all observed reflog candidates; `required` applies the
selected expiry policy. These sets can be partial after failure: always check `is_complete()` or
match `RetentionOutcome`. A complete plan still can't justify a later deletion, because state can
change after planning; the destructive operations rescan under their own isolation guard.

Graph-bearing commits, trees and tags are decoded and identity-checked. Typed blob edges are located
with kind, size and ancestry checks without decoding every blob payload. Planning is therefore not a
full integrity check. Drop the plan promptly: it pins the object-store generation, and its open
handles can keep retired pack storage allocated.

Unknown, malformed, missing or unsupported inputs make the plan incomplete. Partial clones,
precious-object repositories and `gc.recentObjectsHook` are outside the supported subset. Limits
bound names, roots, edges and decoded graph bytes. `RetentionError::kind()` groups recovery into
limit, unsupported, malformed, changed, cancelled or read; inspect the underlying storage cause of a
read failure before deciding to retry.

These operations preserve Git's object relationships and conservative expiry rules; they do not
reproduce every `git gc` policy or read all of its configuration automatically. The caller chooses
the policy and provides isolation, and unsupported cases return errors. Whole-pack protection can
retain more data than Git's per-loose-file expiry boundary.

## Set cutoffs without confusing their boundaries

The application chooses `recent_cutoff`; girt does not read `gc.pruneExpire` for it. With no cutoff,
loose expiry is disabled and packs remain protected. With a cutoff:

- A primary loose file with `mtime <= cutoff` is eligible only when its identity is outside the
  retained closure. The inclusive boundary matches Git prune expiry, including subsecond times.
- A newer loose file becomes a root, retaining its dependencies as well as itself. Age alone never
  establishes that an object is garbage.
- A pack with `mtime >= cutoff` remains protected in its entirety. This boundary is more
  conservative than loose-file expiry.

`.keep` packs and alternate stores are also protected. Girt never deletes or rewrites their storage.
Girt also protects the objects that surviving protected packs depend on, but doesn't count them as
live when classifying reflog records. Before leaving an object out of a replacement because a
protected pack keeps it, girt verifies the protected copy instead of trusting its index entry.
Unsupported pack sidecars can also prevent retirement; known optional `.rev` and `.bitmap` sidecars
can retire with their pack.

Reflog expiry has its own policy. `KeepAll` is the default. `ClassifiedCutoffs` uses the live cutoff
when either endpoint is in the non-reflog live closure and the disconnected cutoff otherwise.
`Configured` uses a resolved `ReflogExpiry` snapshot and Git's independent general and unreachable
rules: the general rule applies to every record, and the unreachable rule also applies if either
endpoint is disconnected. Null endpoints count as live in configured policy.

`ReflogExpiry::from_config` takes a caller-supplied Unix clock. Global defaults are 90 days for
general expiry and 30 days for unreachable history. Reference-pattern sections can override them;
absent matching patterns leave `refs/stash` protected even when global settings request expiry. Only
a limited date syntax is supported; unsupported expressions are errors rather than guesses.

## Establish exclusion before deletion

Implement `MaintenanceIsolation` with a guard that excludes all relevant Git and girt writers,
readers still depending on old data, and repositories using this store as an alternate. Hold that
boundary for the full operation. Girt cannot enforce external relationships; a mutex that only your
own girt calls take is not enough. If your application cannot establish exclusion, return an error
from `acquire` and use read-only planning or additive repacking instead.

This program needs `girt` and `tempfile`. It returns early on non-Unix hosts because composed GC
requires Unix directory-durability support. It gives an old unreferenced loose blob a deterministic
timestamp, then collects it in an exclusively owned disposable repository. Its trivial isolation
implementation is valid only because this fixture has no external participants; it is not a lock
implementation for shared repositories. See the
[GC API](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.collect_garbage)
for the full contract.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, UNIX_EPOCH};

    use girt::retention::RetentionPolicy;
    use girt::{InitKind, ObjectFormat, Repository};

    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )?;
    let cancel = AtomicBool::new(false);

    if cfg!(not(unix)) {
        return Ok(());
    }

    // This fixture is private to this single-threaded example. No external writer, reader or
    // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
    struct PrivateFixture;
    impl girt::retention::MaintenanceIsolation for PrivateFixture {
        type Guard = ();
        fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
            Ok(())
        }
    }

    let id = repo.loose_objects().write_blob(b"old and unreachable")?;
    let hex = id.to_string();
    let path = repo.object_dir().join(&hex[..2]).join(&hex[2..]);
    std::fs::File::open(path)?.set_modified(UNIX_EPOCH + Duration::from_secs(1))?;

    let policy = RetentionPolicy::default().with_recent_cutoff(UNIX_EPOCH + Duration::from_secs(2));
    let report = repo.collect_garbage(&mut PrivateFixture, &policy, &cancel)?;

    assert_eq!(report.pruned.unwrap().deleted, [id]);
    assert!(
        repo.objects()?
            .read(id)?
            .is_none()
    );

    Ok(())
}
```

Metadata fingerprints and pinned file identities detect observed changes but cannot close a race
with an arbitrary writer after the final check. Like Git's pack reader, girt assumes published pack
and index contents are immutable. Atomic rename/deletion leaves pinned handles reading their
original generation. Girt uses positioned reads and owned buffers, avoiding mapped-page faults after
truncation; that does not make in-place mutation supported.

### Map application ownership to the guard lifetime

For a private store used only by cooperating application threads, a mutex guard can hold the
exclusion boundary. Every reader and writer that might depend on old storage must take that mutex.
This does not extend to an external Git process or another repository using the store as an
alternate. If those participants cannot be excluded, return an error from `acquire` rather than a
guard that excludes nothing.

A `MutexGuard` can hold the boundary for the maintenance call, as required by the
[isolation contract](https://docs.rs/girt/latest/girt/retention/trait.MaintenanceIsolation.html). In
that example, the application maps a poisoned mutex to an I/O error before scanning. An application
with an external process protocol can supply its own guard with the same lifetime, provided that
protocol excludes every dependent participant.

Do not reuse an earlier plan as a deletion list after acquiring the guard. The destructive methods
rescan inside it. After a partial failure, inspect the repository while it is still excluded, then
retry under a new guard. Independent `expire_reflogs`, `retire_old_packs` and
`prune_unreachable_loose` calls acquire separate guards, so each later phase observes new state. Use
`collect_garbage` or `run_maintenance` to keep the whole sequence under one guard.

## Publish, verify, retire

Reclamation uses one pinned snapshot for scanning, selection and copying. Reopening between those
steps could copy a different generation than the plan described. Before removing old storage, girt:

1. Writes and synchronizes replacement data.
1. Publishes the replacement and establishes the required directory durability.
1. Decodes every retained object from the replacement or protected copy that will survive, under the
   real read limits.
1. Revalidates input/replacement handles and metadata, then retires eligible old artifacts.

A pack checksum or CRC can establish intact bytes without proving that an index names the correct
object or that a delta chain reconstructs it under the reader's limits. Decoding and hashing the
surviving copies proves that. Individually valid delta chains can exceed limits when combined; the
writer must re-encode them to fit, or verification refuses to retire the old storage after the
replacement is published.

Old pack indexes are removed before their packs. An interrupted sequence can leave an unindexed pack
that readers ignore; being unindexed doesn't justify deleting it without a fresh observation.
Alternate and protected data remain untouched. Object pruning removes only eligible primary loose
files, never working files or arbitrary unknown objects. After any deletion, it also removes
`objects/info/commit-graph` and `objects/info/commit-graphs`, because those derived caches can name
pruned commits. When any pack will retire, retirement first removes `multi-pack-index*` artifacts,
including associated bitmaps, reverse indexes and incremental-chain directories, so Git does not
consult indexes naming removed packs. Git can rebuild these accelerators; queries lose their
speedups until then. No-op standalone prune/expiry can avoid graph traversal after a complete
bounded observation proves no candidates exist; an empty report doesn't prove history is intact.

Destructive reflog expiry, pack retirement and composed maintenance require Unix durability support.
Non-Unix platforms refuse those operations before publication. Standalone loose pruning and additive
repack have their own contracts; do not infer platform support for the whole sequence from one
phase.

## Recover from damaged packs

An ordinary content read can retry a damaged packed entry through other packs or loose copies. Each
fallback must fully decode and identity-verify. A successful rescue proves the returned copy only:
the original pack remains known damaged. Mutating operations reject pinned inputs known to be
damaged; maintenance preserves that pack and its unique objects rather than treating the good
duplicate as permission to erase the damaged artifact.

Damage evidence follows pinned pack/offset identities and reader clones. A separately opened reader
establishes its own evidence; opening another reader is not proof of repair. Restore or rebuild
verified storage, check retained objects, and open a fresh reader after repair. Index repair alone
does not heal damaged pack bytes.

Cancellation, bounds, unsupported types and missing/external delta bases are immediate errors rather
than fallback triggers. Some header damage can present as those errors and therefore will not be
rescued. If no verified copy exists, preserve the damaged-pack path, entry offset and underlying
corruption cause from the source chain for recovery diagnostics.

### Diagnose before attempting reclamation

1. Stop mutations and exclude dependent readers before changing stored artifacts. Keep the damaged
   pack and its index together as recovery evidence.
1. Read the required object through one pinned `Objects` view. A successful fallback proves that
   object's decoded identity, while `verify_packs` can still expose remembered pack damage. Preserve
   every source error, including the artifact path and entry offset.
1. Recover or rebuild storage from verified data. Account for unique objects in the damaged pack;
   one rescued blob does not establish that every retained object has another copy. Maintenance
   refuses known damaged inputs instead of acting as a damaged-pack removal tool.
1. Drop old readers, open a fresh view after repair, verify artifacts and read the retained objects.
   `verify_packs` checks raw-byte integrity, not all object identities, syntax or connectivity. A
   retention plan's blob-header traversal is also not a full integrity check.
1. Make a fresh retention observation under exclusion before any later reclamation. An old complete
   plan or a successful fallback doesn't make deletion safe.

The [retention module](https://docs.rs/girt/latest/girt/retention/) documents the recovery and
verification contracts. Its damaged-pack diagnostic fixture creates corruption before opening a
reader, rescues content from a loose duplicate and keeps the damaged artifact for recovery.

## Interpret partial failures

A failed mutation can leave completed effects. Preserve the report and typed cause; inspect actual
storage while excluded, then make fresh observations under a new guard before retrying.

- `PruneFailure` records confirmed deletions and a possibly uncertain object unlink identity. A
  later commit-graph cleanup failure returns an I/O cause after those deletions, with no uncertain
  object.
- `RetireFailure` records visible replacement publication, durability, removed paths and an
  uncertain artifact. Publication can precede a later verification or sync failure.
- `ExpireFailure` records durable histories, retained obsolete tables, and an uncertain history or
  stack-list replacement. A directory-sync error can occur after replacement becomes visible.
- `MaintenanceFailure` records completed earlier phases; its nested phase failure contains the
  current phase's partial effects. Cancellation does not imply rollback.

When matching `ExpireCause::Incomplete`, use `detail.as_ref()` to borrow its boxed `RetentionError`.
Plans and their diagnostics are `Send + Sync`; source errors do not promise `UnwindSafe` or
`RefUnwindSafe`, so do not infer panic-recovery guarantees from thread safety.

## Reftable compaction and shutdown locks

Reftable stores refs and reflogs in immutable tables selected by `tables.list`. Newer keys override
older ones, with tombstones preserving deletions. Ordinary transactions belong to
`refs::References`. `Table` is a byte codec; `Snapshot` eagerly merges a bounded generation and
remains usable after later publication. `compact` replaces the complete stack without expiring live
reflog records or deleting objects. It holds cooperating-writer locks, publishes the list, then
attempts obsolete-file cleanup. Success can include retained files whose cleanup failed. Ordinary
compaction does not fsync or promise power-loss durability; destructive history expiry uses the
maintenance path instead.

For shutdown, stop accepting new work, request cooperative cancellation and call `locks::cleanup` on
a normal thread. It waits for active lock operations, cancels pending reference-lock waits and, on
Unix, invalidates registered guards and removes only identity-matched transient lock files. A
surviving invalidated edit cannot publish. Inspect per-file failures and preserved replacement paths
even when `cleanup` itself returns `Ok`.

Girt installs no signal handlers. Raw POSIX signal handlers cannot call this allocating, locking,
I/O-performing API; use a normal callback thread. Cleanup is process-wide and does not scan for
stale locks. On unsupported platforms it preserves guard validity for ordinary abort/drop and
reports unsupported cleanup. SIGKILL, crashes and power loss cannot run cleanup. Persistent worktree
`locked` markers and pack `.keep` files are never transient cleanup targets.

### Recover obsolete tables

Keep `Compaction.retained` and its per-path errors when obsolete-file removal fails. Girt exposes no
later cleanup API for these files, and another compaction will not discover them. They remain
unlisted and do not rejoin the stack on the next read.

For manual recovery, establish exclusive control of the repository and honor the writer protocol:
hold `tables.list.lock`, re-read `tables.list`, and confirm each candidate is still unlisted and is
one of the paths recorded in `Compaction.retained` before removing it. Preserve unrecognized files;
a directory listing alone does not establish ownership. The
[compaction report](https://docs.rs/girt/latest/girt/refs/reftable/struct.Compaction.html)
retains the cleanup evidence.

### Recover leftover locks

Inspect each per-file failure and its underlying I/O cause, including girt's unsupported-platform
refusal. On Unix, the old guard is already invalidated even if removal failed, so its abort/drop
path cannot clear the leftover lock. On unsupported platforms, it remains valid for ordinary
abort/drop. The [cleanup report](https://docs.rs/girt/latest/girt/locks/struct.CleanupReport.html)
separates these failures from replacement paths preserved for another owner. Never treat preserved
paths as a list of files to remove.

After a process dies, its locks are outside the live registry. Establish exclusive control and
verify that no writer still owns a leftover lock before manual recovery. Age alone cannot establish
that a lock is abandoned. Inspect unpublished contents before removing it; ordinary acquisition
refuses an existing lock, and `cleanup` does not scan for stale locks. Don't accept new work until
all old writers are joined, because cleanup allows new acquisitions after it returns.

## API reference

- [Retention policy, outcomes, reports and errors](https://docs.rs/girt/latest/girt/retention/)
- [Reftable codec, snapshots and compaction](https://docs.rs/girt/latest/girt/refs/reftable/)
- [Interrupt-safe cleanup in normal thread context](https://docs.rs/girt/latest/girt/locks/)
