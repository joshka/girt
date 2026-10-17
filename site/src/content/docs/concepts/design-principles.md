---
title: Design principles
description: The rules that shape girt's API and behaviour.
---

These principles explain the API's shape: why the caller passes limits, why writing an object
doesn't move a branch, and why unsupported input fails instead of falling back.

## Model Git's concepts directly

girt's types are Git's concepts: [`ObjectId`], [`Tree`], [`Commit`], [`Tag`], references, the index
and [`Repository`]. The API follows these concepts and what its callers need, rather than the shape
of the `git` command line, libgit2 or gix. A Git user can predict what an operation does from Git's
documentation.

## Preserve Git's formats exactly

For every supported feature, girt reads and writes the same bytes Git does, and its tests check this
against the `git` executable. That includes round trips of malformed and unusual input where Git
accepts it. Compressed bytes may differ from Git's output, but identities and decoded content always
agree.

## Refuse instead of approximating

When girt meets something it doesn't support, it returns an error that names it: a repository
extension, an index feature, a protocol capability, a path it can't represent. It doesn't fall back
to something close: a refusal is a limitation the caller can see, while a silent approximation could
corrupt data.

## Make effects explicit

In the core API, each step that changes a repository is a separate call. Writing an object doesn't
make it reachable. Receiving a pack installs objects without moving any reference. Updating a
reference doesn't touch the working tree. The caller decides when each effect happens and which
preconditions it checks first, using conditional reference updates.

The [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) module is a convenience layer
for configured remotes. Its `fetch` installs objects and then updates remote-tracking references and
tags in one call, as `git fetch` does. Its documentation says which references it may change and
what a partial failure leaves behind.

## Bound resources

Operations that read untrusted or unbounded data take explicit budgets: object size, delta depth,
history length, pack count, reference count. Input over the selected budget is refused with an
error. Budgets apply to one operation's inputs. They don't cap total process memory, concurrent
operations or elapsed time, which remain the application's responsibility. The defaults suit
ordinary repositories. Raising them means accepting more work from a repository you trust.

## Never lose the last copy

Operations that delete or rewrite stored data publish and verify the replacement before they retire
the original. The [safety model](/girt/compatibility/safety-model/) describes the rules.

## Grow in narrow, usable steps

girt adds a feature when a real caller needs it, starting with jj, and ships it complete enough to
use. It doesn't add abstractions or options ahead of a caller.

[`ObjectId`]: https://docs.rs/girt/latest/girt/object/enum.ObjectId.html
[`Tree`]: https://docs.rs/girt/latest/girt/tree/struct.Tree.html
[`Commit`]: https://docs.rs/girt/latest/girt/commit/struct.Commit.html
[`Tag`]: https://docs.rs/girt/latest/girt/tag/struct.Tag.html
[`Repository`]: https://docs.rs/girt/latest/girt/repository/struct.Repository.html
