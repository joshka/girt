# girt-cli

`girt` is a command-line tool for inspecting, comparing and benchmarking Git repositories with the
[girt](https://github.com/joshka/girt) library. It is not a replacement for Git: it exposes what the
library does today, under names that follow girt's concepts rather than Git's plumbing commands.

```sh
cargo install girt-cli
girt repo info
girt log --limit 5
girt diff --patch v1 main
```

## Output

Default output is line-oriented and meant for both people and scripts: full object IDs, one record
per line, stable field order. `--git` makes a command print exactly what its Git equivalent prints,
so the two tools can be diffed:

```sh
diff <(girt --git tree list --recursive HEAD) <(git ls-tree -r HEAD)
hyperfine 'girt object batch --check < ids' 'git cat-file --batch-check < ids'
```

`tests/git_parity.rs` checks these pairings against `git` in a scratch repository.

| girt                                                                | Git equivalent for `--git`                                                                           |
| ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `repo info`                                                         | `git rev-parse --git-dir --git-common-dir --show-toplevel --is-bare-repository --show-object-format` |
| `init`                                                              | `git init`                                                                                           |
| `object id` / `hash` / `show` / `info` / `cat` / `exists` / `batch` | `git rev-parse --verify` / `hash-object` / `cat-file -p` / `-t`, `-s` / raw / `-e` / `--batch`       |
| `tree list`                                                         | `git ls-tree`                                                                                        |
| `ref list` / `show` / `set` / `delete` / `log`                      | `git for-each-ref` / `rev-parse` / `update-ref` / `update-ref -d` / `reflog show`                    |
| `log`                                                               | `git log`                                                                                            |
| `history count` / `merge-base` / `contains`                         | `git rev-list --count` / `merge-base` / `merge-base --is-ancestor`                                   |
| `diff` / `--renames` / `--patch`                                    | `git diff-tree -r --name-status` / `-M` / `git diff`                                                 |
| `index list`                                                        | `git ls-files`                                                                                       |
| `status`                                                            | `git status --porcelain`                                                                             |
| `ignore check`                                                      | `git check-ignore -v`                                                                                |
| `checkout`                                                          | `git read-tree -um` (HEAD does not move)                                                             |
| `worktree list`                                                     | `git worktree list`                                                                                  |
| `config get` / `list`                                               | `git config --get` / `--list`                                                                        |
| `remote list` / `heads`                                             | `git remote -v` / `git ls-remote`                                                                    |
| `clone` / `fetch` / `push`                                          | no `--git` output                                                                                    |
| `pack verify` / `reindex`, `maintenance`                            | `git index-pack` for `reindex` only                                                                  |

## Errors and logging

Errors print as [color-eyre](https://docs.rs/color-eyre) reports. `-v`, `-vv` and `-vvv` (or
`RUST_LOG`) log girt's [tracing](https://docs.rs/tracing) spans to stderr; at debug level the active
spans are also captured in error reports.

## Limits

These come from the library and are listed in each command's `--help`:

- Revisions use Git's syntax (`main~2`, `v1^{tree}`, `HEAD:path`, `main@{1}`, `@{u}`, `@{push}`),
  and `log` and `history count` take ranges (`A..B`, `A...B`, `^A`). Pseudo-references other than
  `HEAD` (such as `ORIG_HEAD`), dated reflog selectors, `@{-N}`, `:path` index lookups and `:/text`
  searches are refused. See the library's `revision` module.
- Ranges walk the excluded side's whole history, unlike Git, which stops early, so `log A..B` near
  the root of a long history is slower than Git.
- Revisions accept unique hexadecimal prefixes of at least four digits. Ref names take precedence
  over short IDs; Git's ambiguous-name warning is not reproduced.
- Abbreviated IDs honor `core.abbrev` (automatic by default) and lengthen collisions across loose
  objects, indexed packs and alternates. Unindexed packs and promised objects not present locally do
  not participate. Each output operation retains loose buckets as they are first inspected;
  concurrent writes can invalidate a previously unique abbreviation.
- `status` and `checkout` compare raw bytes on macOS and Linux: no attributes, filters, EOL
  conversion or `core.filemode`. macOS paths must be ASCII. `checkout` drops derived index caches,
  including fsmonitor and untracked caches, for Git to rebuild. Split/sparse indexes, resolve-undo
  data and unknown extensions are refused.
- `diff --patch` uses a plain Myers diff, so ambiguous hunks can align differently from Git.
- SSH is supported by `fetch` and `push` but not yet by `clone` or `remote heads`.
- `maintenance run` deletes data. It requires `--yes`, and no other process may use the repository
  while it runs.
