---
title: The Git model
description: The Git concepts girt exposes and the types that represent them.
---

:::note[Work in progress]
This page is an outline. It will explain each concept for readers who know Git as a user, not its
internals, and link to the docs.rs items that implement it.
:::

| Git concept             | girt                                       |
| ----------------------- | ------------------------------------------ |
| Object ID               | [`ObjectId`], SHA-1 or SHA-256             |
| Blob, tree, commit, tag | [`Tree`], [`Commit`], [`Tag`]              |
| Object storage          | [`Objects`], [`LooseObjects`], [`pack`]    |
| References, reflogs     | [`refs`]                                   |
| Index                   | [`index`]                                  |
| Repository              | [`Repository`]                             |
| Remotes, refspecs       | [`remote`]                                 |

## Outline

- **Objects and identity:** content addressing, object kinds, SHA-1 and SHA-256, and why girt never
  converts between them.
- **Loose and packed storage:** how an object is found, pack snapshots, and what a reader sees when
  another process repacks.
- **Trees and paths:** entry modes, raw path bytes and ordering, gitlinks.
- **Commits and history:** parents, ancestry walks, merge bases, shallow boundaries.
- **References:** direct and symbolic refs, the files and reftable backends, reflogs, peeling.
- **The index:** stages and conflicts, versions 2–4, extensions girt preserves or refuses.
- **Repositories:** the Git directory, common directory, worktrees, configuration layers,
  alternates.
- **Remotes:** refspecs, remote-tracking refs, what a fetch installs and what it leaves to the
  caller.

[`ObjectId`]: https://docs.rs/girt/latest/girt/enum.ObjectId.html
[`Tree`]: https://docs.rs/girt/latest/girt/struct.Tree.html
[`Commit`]: https://docs.rs/girt/latest/girt/struct.Commit.html
[`Tag`]: https://docs.rs/girt/latest/girt/struct.Tag.html
[`Objects`]: https://docs.rs/girt/latest/girt/struct.Objects.html
[`LooseObjects`]: https://docs.rs/girt/latest/girt/struct.LooseObjects.html
[`pack`]: https://docs.rs/girt/latest/girt/pack/index.html
[`refs`]: https://docs.rs/girt/latest/girt/refs/index.html
[`index`]: https://docs.rs/girt/latest/girt/index/index.html
[`Repository`]: https://docs.rs/girt/latest/girt/struct.Repository.html
[`remote`]: https://docs.rs/girt/latest/girt/remote/index.html
