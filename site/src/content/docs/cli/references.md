---
title: References
description: Read names and reflogs and make guarded reference updates.
---

`ref list` lists names, `show` follows symbolic references, and `log` reads reflogs. `set` and
`delete` change references; use `--expect` or `--create-only` where applicable to guard against a
stale value. Run `girt help ref set` before writing.

## Inspect branch names

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

<!-- markdownlint-disable MD010 -->

```text
$ girt ref list
e5d83fbd011e44793ba5f60d5091bb569e12352f refs/heads/main
$ git for-each-ref
e5d83fbd011e44793ba5f60d5091bb569e12352f commit	refs/heads/main
```

<!-- markdownlint-enable MD010 -->

The default girt record omits the object kind. Use `girt --git ref list` for Git's format.
`ref show` corresponds to `git rev-parse`, `ref set` to `git update-ref`, `ref delete` to
`git update-ref -d`, and `ref log` to `git reflog show`.

## Library API and limits

The commands open
[Repository::references](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.references)
and use [References](https://docs.rs/girt/latest/girt/refs/struct.References.html) for lookup,
iteration and edits. Updates compose
[RefEdit](https://docs.rs/girt/latest/girt/refs/struct.RefEdit.html) with
[Expected](https://docs.rs/girt/latest/girt/refs/enum.Expected.html) guards; log reading uses
[Reflog](https://docs.rs/girt/latest/girt/refs/enum.Reflog.html).

`set` follows symbolic references, requires the target object to exist and refuses non-commit branch
targets. Logged updates use the configured committer identity and UTC timestamps. Deleting a
reference also deletes its reflog. See [Read and update refs](/girt/guides/refs/) for transaction
and concurrency guarantees; a CLI write has the same obligations as a library write.
