---
title: Command-line tool
description: Install the girt command, inspect repositories and compare its output with Git.
---

`girt` is a command-line front end to the library. Use it to look inside a repository, try an
operation before writing code, or compare girt with Git on real data. It is not a replacement for
Git: each command exposes what the library supports today.

## Install

```sh
cargo install girt-cli
```

The binary is called `girt`. It includes the HTTP and SSH transports and the `tracing` feature.

## Commands

Commands are grouped by Git concept instead of Git's plumbing names:

| Group                     | Commands                                                              |
| ------------------------- | --------------------------------------------------------------------- |
| Repository                | `repo info`, `init`, `clone`                                          |
| Objects and trees         | `object hash`, `show`, `info`, `cat`, `exists`, `batch`; `tree list`  |
| References                | `ref list`, `show`, `set`, `delete`, `log`                            |
| History                   | `log`, `history count`, `merge-base`, `contains`                      |
| Diffs                     | `diff` with `--names`, `--renames`, `--copies` or `--patch`           |
| Index and working tree    | `index list`, `status`, `ignore check`, `checkout`, `worktree`        |
| Configuration and remotes | `config`, `remote list`, `add`, `show`, `heads`; `fetch`, `push`      |
| Storage                   | `pack list`, `verify`, `reindex`; `maintenance plan`, `repack`, `run` |

Run `girt help <command>` for arguments, effects and limits. `-C <path>` runs in another directory,
as with Git.

```sh
girt repo info
girt log --limit 5
girt diff --renames v0.9.0 main
girt object batch --check < ids.txt
```

## Output and `--git`

Default output is line-oriented: full object IDs, one record per line and a stable field order, so
it works in pipelines. `--git` makes a command print exactly what its Git equivalent prints:

```sh
diff <(girt --git tree list --recursive HEAD) <(git ls-tree -r HEAD)
hyperfine 'girt object batch --check < ids' 'git cat-file --batch-check < ids'
```

The [CLI README](https://github.com/joshka/girt/blob/main/girt-cli/README.md) maps every command to
its Git equivalent, and a parity test checks those pairs against `git` on every change.

## Live progress

Clone, fetch, push and checkout show live phases and available object counts on stderr when it is
connected to a terminal. Known totals include percentages; server messages appear as `remote:`
lines. Redraws are limited to about ten per second, with a final line for each phase. Checkout
counts include file and directory operations. Standard output keeps its existing record format.

Use `-q` or `--quiet` to suppress progress, including server progress messages. `--git` also
suppresses progress, as does redirecting stderr (for example, `2>transfer.log`). Errors and
explicitly requested logging remain available.

Pack verification, reindexing and maintenance show elapsed activity and final report totals where
available. The library does not yet expose their intermediate counters or network byte counters, so
these phases do not show invented percentages or throughput. Remaining hooks are tracked in
[the progress counter issue](https://github.com/joshka/girt/issues/371).

```sh
girt clone https://github.com/joshka/girt
girt --quiet fetch
girt checkout HEAD 2>checkout.log
```

## Errors and logging

Errors print as reports with the chain of causes. `-v`, `-vv` and `-vvv` (or `RUST_LOG`) log girt's
operation spans to stderr; at debug level the active spans also appear in error reports.

## Current limits

These come from the library; each command's help repeats the ones that apply to it.

- Revisions are full object IDs or reference names; short IDs and expressions such as `HEAD~1` are
  in progress.
- Abbreviated IDs in `--git` output are always seven digits, so compare with `git -c core.abbrev=7`
  on larger repositories.
- `status` and `checkout` compare raw bytes on macOS and Linux, without attributes, filters, line
  ending conversion or `core.filemode`. `checkout` refuses indexes that carry fsmonitor or
  untracked-cache data.
- `diff --patch` uses a plain Myers diff, so ambiguous hunks can line up differently from Git.
- `clone` and `remote heads` do not yet use SSH.
- `maintenance run` deletes data. It requires `--yes`, and nothing else may use the repository while
  it runs.

See [Limits and trust](/girt/guides/limits-and-trust/) for the library's general boundaries.
