---
title: Diffs
description: Compare trees, detect renames and copies and print patches.
---

`diff A B` compares two commit or tree revisions. `--names` is the default; `--renames` detects
renames, `--copies` also considers copies from modified files, and `--patch` writes a unified diff.
Run `girt help diff` for the supported combinations.

## Find changed paths

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

<!-- markdownlint-disable MD010 -->

```text
$ girt --git diff HEAD~1 HEAD
M	notes.txt
$ git diff-tree --no-commit-id -r --name-status HEAD~1 HEAD
M	notes.txt
```

<!-- markdownlint-enable MD010 -->

`M` identifies the modified file. Use `git diff-tree -M` for the rename comparison and
`git diff HEAD~1 HEAD` for the patch equivalent. Default girt names output separates fields with
spaces; `--git` uses Git's tab-separated form.

## Library API and limits

Plain comparisons use
[Objects::compare_trees](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.compare_trees).
Rename and copy detection use
[Objects::diff_trees_with](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.diff_trees_with)
with [rewrites::Options](https://docs.rs/girt/latest/girt/rewrites/struct.Options.html). Patch
output uses
[Objects::write_patch_with](https://docs.rs/girt/latest/girt/objects/struct.Objects.html#method.write_patch_with)
and repository diff attributes.

Patches use Myers diff, so ambiguous hunks can align differently from Git. A warning reports when
limits skip exhaustive rename detection; the remaining comparison still has useful path changes. See
[Diffs and renames](/girt/guides/diffs-and-renames/) for attribute policy, similarity and limits.
