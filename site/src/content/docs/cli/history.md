---
title: History and blame
description: Walk commits, test ancestry and attribute file lines.
---

`log` shows commits, with `--limit` and `--oneline` for shorter listings. `history count` counts
reachable commits; `merge-base` finds common ancestors and `contains` tests ancestry. `blame`
attributes a literal repository-relative path to commit ranges. Run `girt help history` and
`girt help blame` for arguments.

:::caution[Experimental CLI]
The girt CLI is not a replacement for Git. It exercises the library's Git operations and provides a
testing surface for performance and byte-for-byte comparisons with Git. Command names and parameters
may change; the library is the core deliverable.
:::

## Count commits and attribute lines

Use the repository from [Getting started with the CLI](/girt/cli/getting-started/).

```text
$ girt history count HEAD
2
$ girt blame notes.txt
20e181280c52172d1d2916d4f225a928d5d05a3c 1 2 1 notes.txt
e5d83fbd011e44793ba5f60d5091bb569e12352f 3 1 3 notes.txt
```

The history has two commits. Each blame record contains a commit ID, final start line, line count,
original start line and original path. The first commit introduced lines 1–2; the second introduced
line 3. Starts are one-based. Blame prints ranges rather than file contents or per-line metadata.
`--git` is refused for blame.

`history contains` prints `yes` or `no` and exits 0 or 1; `--git` suppresses its text. The
corresponding Git operations are `git log`, `git rev-list --count`, `git merge-base`,
`git merge-base --is-ancestor` and `git blame --line-porcelain`.

## Library API and limits

The CLI resolves selections with
[Selection](https://docs.rs/girt/latest/girt/revision/struct.Selection.html) and uses
[Objects](https://docs.rs/girt/latest/girt/objects/struct.Objects.html) methods `walk_with`,
`merge_bases_with` and `is_ancestor_with`. Log lists commits newest committer date first, as
`git log` does. Blame calls
[Repo::blame](https://docs.rs/girt/latest/girt/repo/struct.Repo.html#method.blame).

`log` and `history count` accept ranges such as `main~1..main`. The excluded side is walked in full,
which can be slower than Git near the root of a long history. Unsupported revision syntax is
refused. See [Read objects and history](/girt/guides/objects-and-history/) for shallow history and
resource limits.
