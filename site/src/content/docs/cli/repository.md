---
title: Repository
description: Inspect repository layout and initialize Git storage.
---

`repo info` reports object format, reference backend, paths, shallow state and ongoing operations.
`init` creates a new directory containing a bare or working-tree repository; it refuses an existing
destination and does not read Git templates. Use `girt help init` for format and backend choices.

## Inspect the layout

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

These commands query the same layout. The absolute top-level path below is shown as
`/path/to/cli-example`; the other fields are the Git directory, common directory, bare flag and
object format.

```text
$ girt --git repo info
.git
.git
/path/to/cli-example
false
sha1
$ git rev-parse --git-dir --git-common-dir --show-toplevel --is-bare-repository --show-object-format
.git
.git
/path/to/cli-example
false
sha1
```

Without `--git`, `repo info` labels each field and includes more state. Git's `rev-parse` has no
single equivalent for all of those fields. The initialization equivalent is `git init`, with
matching `--bare`, `--object-format` and `--initial-branch` options.

## Library API

`repo info` uses [Repository](https://docs.rs/girt/latest/girt/repository/struct.Repository.html)
accessors including `object_format`, `reference_backend`, `git_dir`, `common_dir`, `worktree` and
`shallow_roots`. `init` calls
[Repository::init_with_options](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.init_with_options).
The CLI loads environment and configuration before opening the repository; library callers choose
those inputs explicitly. See [Open a repository](/girt/guides/open-a-repository/).
