---
title: Design principles
description: The rules that shape girt's API and behaviour.
---

These principles explain the API's shape: why the caller passes limits, why writing an object
doesn't move a branch, and why unsupported input fails instead of falling back.

## Model Git's concepts directly

girt's types are Git's concepts: [`ObjectId`], [`Tree`], [`Commit`], [`Tag`], references, the index
and [`Repository`]. The API is designed from these concepts and from what its consumers need. It
doesn't imitate the `git` command line, libgit2 or gix. A Git user can predict what an operation
does from Git's documentation.

## Preserve Git's formats exactly

For every supported feature, girt reads and writes the same bytes Git does, and its tests check this
against the `git` executable. That includes round trips of malformed and unusual input where Git
accepts it. Compressed bytes may differ from Git's output, but identities and decoded content always
agree.

## Refuse instead of approximating

When girt meets something it doesn't support, it returns an error that names it: a repository
extension, an index feature, a protocol capability, a path it can't represent. It never silently
does something close. A refusal is a known limitation. A silent approximation could corrupt data.

## Make effects explicit

Each step that changes a repository is a separate call. Writing an object doesn't make it reachable.
Fetching doesn't move a branch. Updating a reference doesn't touch the working tree. The caller
decides when each effect happens and which preconditions it checks first, using conditional
reference updates.

## Bound resources

Operations that read untrusted or unbounded data take explicit limits: object size, delta depth,
history length, pack count, reference count. The defaults suit ordinary repositories. A hostile or
enormous repository produces an error rather than exhausting memory or time.

## Never lose the last copy

Operations that delete or rewrite stored data publish and verify the replacement before they retire
the original. The [safety model](/girt/compatibility/safety-model/) describes the rules.

## Grow in narrow, usable steps

girt adds features as small, complete increments driven by real call sites, starting with jj's. It
avoids speculative abstraction and configuration.

[`ObjectId`]: https://docs.rs/girt/latest/girt/enum.ObjectId.html
[`Tree`]: https://docs.rs/girt/latest/girt/struct.Tree.html
[`Commit`]: https://docs.rs/girt/latest/girt/struct.Commit.html
[`Tag`]: https://docs.rs/girt/latest/girt/struct.Tag.html
[`Repository`]: https://docs.rs/girt/latest/girt/struct.Repository.html
