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

## Color

The global `--color auto|always|never` option controls help, diagnostics and human output. The
default, `auto`, decides separately for stdout and stderr: a non-empty `NO_COLOR` disables color,
then a non-empty `CLICOLOR_FORCE` other than `0` forces it, then `CLICOLOR=0` disables it.
Otherwise, color requires a terminal and `TERM` other than `dumb`. An explicit `always` or `never`
overrides the environment.

Status uses green for staged changes and red for unstaged and untracked changes. Patches use bold
headers, cyan hunk headers, green additions and red deletions; log IDs are yellow. `--git` output
remains uncolored even with `--color always`. Color settings do not change when progress is
displayed.

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

Errors print as [color-eyre](https://docs.rs/color-eyre) reports. `-v`, `-vv` and `-vvv` log girt's
[tracing](https://docs.rs/tracing) spans to stderr; at debug level the active spans are also
captured in error reports. `GIRT_LOG` takes
[`EnvFilter` directives](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives)
(for example `GIRT_LOG=girt=debug`) and overrides `-v`; an invalid value is an error. `RUST_LOG` is
not read, so filters set for other tools don't affect girt.

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
- `clone`, `remote heads`, `fetch` and `push` support SSH on macOS/Linux using system OpenSSH:
  `ssh://[user@]host[:port]/path` or `[user@]host:path`. Normal SSH config, identities, agents and
  host trust apply. Command selection is `GIT_SSH_COMMAND`, `core.sshCommand`, `GIT_SSH`, then `ssh`
  on PATH. Configured command strings execute through the shell, so trust the configuration.
  Terminal prompts/job control are unavailable; use an agent or askpass. Non-OpenSSH variants,
  percent-encoded SSH URLs, query/fragment fields and paths starting with `~` are unsupported.
- HTTP(S) `clone` and `remote heads` still use anonymous access, without credential helpers. `fetch`
  and `push` use the library's configured credential helpers, including `store`; they decline
  interactive credential prompts. Adding helpers to clone/discovery requires sharing the workflow
  adapter's helper selection and authentication retry policy.
- `maintenance run` deletes data. It requires `--yes`, and no other process may use the repository
  while it runs.
