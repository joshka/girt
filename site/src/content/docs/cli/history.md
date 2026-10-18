---
title: History and blame
description: Walk commits, test ancestry and attribute file lines.
---

`log` shows commits, with `--limit` and `--oneline` for shorter listings. `history count` counts
reachable commits; `merge-base` finds common ancestors and `contains` tests ancestry. `blame`
attributes a literal repository-relative path to commit ranges. Run `girt help history` and
`girt help blame` for arguments.

## Count commits and attribute lines

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

```text
$ girt history count HEAD
2
$ git rev-list --count HEAD
2
$ girt blame notes.txt
20e181280c52172d1d2916d4f225a928d5d05a3c 1 2 1 notes.txt
e5d83fbd011e44793ba5f60d5091bb569e12352f 3 1 3 notes.txt
```

The Git equivalent for blame is `git blame --line-porcelain notes.txt`. Its range headers identify
`20e181280c52172d1d2916d4f225a928d5d05a3c` as the origin of lines 1–2, and
`e5d83fbd011e44793ba5f60d5091bb569e12352f` as the origin of line 3. girt's fields are commit ID,
final start, line count, original start and original path; it does not print the file contents or
Git's per-line metadata. `--git` is refused for blame.

`girt log` corresponds to `git log`; `history merge-base` to `git merge-base`, and
`history contains` to `git merge-base --is-ancestor`. Contains prints `yes` or `no` by default and
exits 0 or 1; `--git` suppresses the text.

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
