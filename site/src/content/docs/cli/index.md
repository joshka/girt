---
title: CLI overview
description: Use the girt CLI to exercise the library on real repositories.
---

`girt` is a command-line front end to the library. Use it to look inside a repository, try an
operation before writing code, or compare girt with Git on real data. Running the two tools side by
side gives you a direct check of the library's behavior and a starting point for a Rust program. It
is not a replacement for Git: each command exposes what the library supports today.

## Install

```sh
cargo install girt-cli
```

The binary is called `girt`. It includes the HTTP and SSH transports and the `tracing` feature.

## Explore the commands

Start with [Comparing with Git](/girt/cli/comparing-with-git/) to create a small repository and
compare observable results. The command pages use that repository and connect each operation to its
library API:

- [Repository](/girt/cli/repository/): inspect layout and initialize storage.
- [Objects and trees](/girt/cli/objects-and-trees/): resolve, hash and read stored data.
- [References](/girt/cli/references/): inspect and update names and reflogs.
- [History and blame](/girt/cli/history/): walk commits, query ancestry and attribute lines.
- [Diffs](/girt/cli/diffs/): compare trees, detect rewrites and print patches.
- [Index and working tree](/girt/cli/index-and-working-tree/): inspect tracked state and checkout.
- [Configuration and remotes](/girt/cli/configuration-and-remotes/): inspect policy and endpoints.
- [Fetch, push and clone](/girt/cli/fetch-push-clone/): exercise transfer workflows.
- [Storage and maintenance](/girt/cli/storage-and-maintenance/): inspect packs and plan retention.

Run `girt help <command>` for arguments, effects and limits. Global `-C <path>` runs in another
directory, `--color auto|always|never` controls color, `--git` selects the available Git-compatible
output, and `-q` suppresses progress. Default output uses full IDs and stable record fields; there
is no global JSON output mode. See the output and logging sections below for the remaining flags.

## Shell completion

Add one line to your shell's startup file. Tab then completes commands, options and values read from
the current repository: revisions and ref names, remote names, linked worktree names and config
keys.

```sh
source <(girt completions zsh)     # ~/.zshrc
source <(girt completions bash)    # ~/.bashrc
girt completions fish | source     # ~/.config/fish/config.fish
```

The script is a small hook that calls back into `girt` on each Tab, so completions stay in step with
the installed version. Completing a value reads only refs, config and worktree registrations, never
objects; outside a repository it offers no values. `-C <path>` is not applied while completing.
`girt completions` also supports `elvish` and `powershell`, and `COMPLETE=<shell> girt` prints the
same script.

Packagers who need a fixed file can use `girt completions --static <shell>`, which completes
commands and options but not repository values.

## Man pages

`girt man` prints `girt.1`. `girt man --out <dir>` writes `girt.1` and one page per subcommand
(`girt-object.1`, `girt-object-show.1`, ...) for installing into a `man1` directory:

```sh
girt man --out ~/.local/share/man/man1
man girt-diff
```

## Output and `--git`

Default output is line-oriented: full object IDs, one record per line and a stable field order, so
it works in pipelines. `--git` selects Git-compatible output where the command supports it:

```sh
diff <(girt --git tree list --recursive HEAD) <(git ls-tree -r HEAD)
hyperfine 'girt object batch --check < ids' 'git cat-file --batch-check < ids'
```

The [CLI README](https://github.com/joshka/girt/blob/main/girt-cli/README.md) maps every command to
its Git equivalent, and a parity test checks those pairs against `git` on every change.

## Color

Use the global `--color auto|always|never` option to control help, diagnostics and human output. The
default, `auto`, decides separately for stdout and stderr. A non-empty `NO_COLOR` disables color;
otherwise a non-empty `CLICOLOR_FORCE` other than `0` forces it, then `CLICOLOR=0` disables it.
Without those overrides, color requires a terminal and `TERM` other than `dumb`. An explicit
`always` or `never` overrides the environment.

Status colors staged changes green and unstaged and untracked changes red. Patches use bold headers,
cyan hunk headers, green additions and red deletions; log commit IDs are yellow. `--git` output is
always uncolored. Color settings do not affect progress display.

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

Errors print as reports with the chain of causes. `-v`, `-vv` and `-vvv` log girt's operation spans
to stderr; at debug level the active spans also appear in error reports. `GIRT_LOG` takes tracing
filter directives (for example `GIRT_LOG=girt=debug`) and overrides `-v`; an invalid value is an
error. `RUST_LOG` is not read, so filters set for other tools don't affect girt.

## Current limits

These come from the library; each command's help repeats the ones that apply to it.

- Revisions support names, IDs, ancestry expressions (`HEAD~1`), peeling (`HEAD^{tree}`), tree paths
  (`HEAD:notes.txt`) and numeric reflog selectors. `log` and `history count` also accept ranges.
  Pseudo-references other than `HEAD`, dated reflog selectors, `@{-N}`, index lookups and message
  searches are refused. Reference names take precedence over short IDs; Git's ambiguous-name warning
  is not reproduced.
- Abbreviated IDs honor `core.abbrev` (automatic by default), lengthening collisions across loose
  objects, indexed packs and alternates. Unindexed packs and objects absent locally do not
  participate. Each output operation retains loose buckets as they are first inspected, so
  concurrent writes can invalidate uniqueness.
- `status` and `checkout` compare raw bytes on macOS and Linux, without attributes, filters, line
  ending conversion or `core.filemode`. `checkout` drops derived fsmonitor and untracked caches for
  Git to rebuild; split/sparse indexes, resolve-undo data and unknown extensions are refused. macOS
  paths must be ASCII.
- `diff --patch` uses a plain Myers diff, so ambiguous hunks can line up differently from Git.
- All four transfer commands (`clone`, `remote heads`, `fetch`, `push`) support SSH on macOS/Linux.
  OpenSSH configuration and agents apply; terminal prompts and job control are unavailable. See
  [CLI transports](/girt/guides/credentials-and-transports/#cli-transfers) for URL forms, configured
  SSH commands and authentication limits.
- HTTP(S) clone, fetch and push use configured credential helpers, without interactive credential
  prompts. `remote heads` uses anonymous HTTP discovery.
- `maintenance run` deletes data. It requires `--yes`, and nothing else may use the repository while
  it runs.

See [Limits and trust](/girt/guides/limits-and-trust/) for the library's general boundaries.
