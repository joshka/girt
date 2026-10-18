---
title: Storage and maintenance
description: Inspect pack storage and plan retention before deleting data.
---

`pack list` describes packs, `verify` checks their checksums and objects, and `reindex` rebuilds a
pack index. `maintenance plan` computes retention without writing, `repack` adds a pack without
deleting old storage, and `run` can expire reflogs, retire packs and prune loose objects. Run
`girt help maintenance run` before selecting destructive phases.

## Inspect retention and storage

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

```text
$ girt maintenance plan
roots 6
retained 6
reachable 6
unreachable-loose 0
unreachable-packed 0
protected-packs 0
alternates 0
$ git count-objects -v
count: 6
size: 24
in-pack: 0
packs: 0
size-pack: 0
prune-packable: 0
garbage: 0
size-garbage: 0
```

The six retained objects are two commits, two trees and two blobs. Git's storage count also sees six
loose objects; its disk-size fields depend on filesystem allocation. `count-objects` is a storage
inventory, not an equivalent retention plan. There is no exact Git equivalent for girt's maintenance
output or policy. `git verify-pack` checks an individual indexed pack, and `git index-pack <pack>`
corresponds to `girt pack reindex <pack>`.

## Library API and safety

Planning calls
[Repository::plan_retention](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.plan_retention)
with a [RetentionPolicy](https://docs.rs/girt/latest/girt/retention/struct.RetentionPolicy.html).
Pack verification uses
[Objects::verify_packs](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.verify_packs);
index rebuilding uses
[pack::rebuild_index](https://docs.rs/girt/latest/girt/pack/fn.rebuild_index.html). Repacking and
destructive phases use [retention](https://docs.rs/girt/latest/girt/retention/index.html) plans and
explicit safety capabilities.

`maintenance run` requires `--yes` to perform selected phases. That flag asserts that no other
process reads or writes the repository; girt has no repository-wide lock that supplies this
isolation. Without it the command only prints the plan and proposed phases. Recent objects and packs
are protected by the CLI's two-week grace period; reflog expiry uses 90 days for reachable records
and 30 for unreachable records. See [Maintenance and GC](/girt/guides/maintenance/) before running
deletion. `--git` is ignored for maintenance, pack list and verification.
