---
title: Objects and trees
description: Resolve revisions and inspect stored Git objects.
---

`object id` resolves a revision; `hash` computes an ID and optionally writes the object. `show`,
`info`, `cat` and `exists` inspect an object; `batch` reads revisions from stdin. `list` enumerates
objects and `count` summarizes storage. `tree list` lists entries, optionally recursively. Run
`girt help object` and `girt help tree list` for flags.

## Inspect a committed file

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

<!-- markdownlint-disable MD010 -->

```text
$ girt tree list HEAD
100644 blob 4cb29ea38f70d7c61b2a3a25b02e3bdf44905402	notes.txt
$ git ls-tree HEAD
100644 blob 4cb29ea38f70d7c61b2a3a25b02e3bdf44905402	notes.txt
$ girt object info HEAD:notes.txt
4cb29ea38f70d7c61b2a3a25b02e3bdf44905402 blob 14
$ git cat-file -s HEAD:notes.txt
14
```

<!-- markdownlint-enable MD010 -->

The tree record contains the mode, kind, blob ID and path. `object info` adds the payload size in
bytes; Git splits kind and size into `cat-file -t` and `cat-file -s`. `object show` corresponds to
`git cat-file -p`, `object hash` to `git hash-object`, and `object exists` to `git cat-file -e`.
`object batch --check` corresponds to `git cat-file --batch-check`.

## Library API and limits

Revision lookup uses
[Repo::rev_parse](https://docs.rs/girt/latest/girt/repo/struct.Repo.html#method.rev_parse). Object
reads use [Objects](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) (`read_with`,
metadata and enumeration methods); hashing uses
[ObjectId](https://docs.rs/girt/latest/girt/object/enum.ObjectId.html) and writes use
[LooseObjects](https://docs.rs/girt/latest/girt/loose/struct.LooseObjects.html). Tree traversal
decodes [Tree](https://docs.rs/girt/latest/girt/tree/struct.Tree.html) entries from the object
store.

`hash --write` changes storage; plain `hash` does not. Missing objects and ambiguous prefixes fail
explicitly. Multi-pack indexes are not read; enumeration uses individual pack indexes.
`object count --git` omits Git's garbage fields and refuses orphaned indexes. See
[Read objects and history](/girt/guides/objects-and-history/) for the object model and limits.
