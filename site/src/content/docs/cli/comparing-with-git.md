---
title: Comparing with Git
description: Build a small repository and compare library behavior through the CLI.
---

Run girt next to Git to check the library on the same stored data. Begin with read-only commands;
use disposable copies for operations that change references, the index, files or storage. The CLI is
a realistic way to experiment with Git-like operations before composing the same APIs in Rust. It
exposes the library's current support rather than providing a replacement Git workflow.

## Create the example repository

Install `girt` with `cargo install girt-cli`. Run this setup in a new scratch directory. It uses
only Git's executable and creates two commits with fixed identities and dates, so the IDs below are
reproducible. `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_NOSYSTEM` isolate the example from user and system
configuration; keep these overrides in this example shell.

```sh
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Example GIT_COMMITTER_NAME=Example
export GIT_AUTHOR_EMAIL=example@example.com GIT_COMMITTER_EMAIL=example@example.com
export GIT_AUTHOR_DATE=2026-01-01T12:00:00+0000
export GIT_COMMITTER_DATE=2026-01-01T12:00:00+0000
git init --object-format=sha1 -b main cli-example
cd cli-example
printf 'one\ntwo\n' > notes.txt
git add notes.txt
git -c commit.gpgsign=false commit -m 'Add notes'
printf 'one\ntwo\nthree\n' > notes.txt
git -c commit.gpgsign=false commit -am 'Extend notes'
```

The command pages show output captured from this repository. Paths vary with its location. Commands
run from `cli-example` unless stated otherwise; `girt -C cli-example ...` lets you run from its
parent directory.

## Compare matching output

Default output favors full IDs and simple records. Use `--git` when the command provides a matching
Git format. In Bash or Zsh, compare the recursive tree listing directly:

```sh
diff <(girt --git tree list --recursive HEAD) <(git ls-tree -r HEAD)
```

An empty diff and exit status 0 mean these listings match. Both commands produce this record:

<!-- markdownlint-disable MD010 -->

```text
100644 blob 4cb29ea38f70d7c61b2a3a25b02e3bdf44905402	notes.txt
```

<!-- markdownlint-enable MD010 -->

This checks tree entries, modes and IDs; it does not establish every library operation's
compatibility. For a different repository, compare the same revision and options and keep its
contents fixed during the comparison.

## Read differences

A formatting difference can be intentional. `girt ref list` prints an ID and name, while
`git for-each-ref` includes the object kind; `girt --git ref list` adds that field. Blame prints
structured ranges and refuses `--git`. Clone, fetch and push retain girt's result format with
`--git`, which only suppresses their progress. Maintenance has no exact Git equivalent.

An error can mark an unsupported operation rather than a different result: for example, dated reflog
selectors are refused. Read the error chain and `girt help <command>` before treating the outputs as
comparable. For supported inputs, an unexplained difference is useful evidence for a bug report:
include both commands, versions, exit statuses and a reproducible repository without private data.
See [Limits and trust](/girt/guides/limits-and-trust/) for library boundaries.

The command pages link the APIs used by the CLI. Follow those links for contracts, limits and caller
obligations when moving the experiment into Rust. The
[CLI parity tests](https://github.com/joshka/girt/blob/main/girt-cli/tests/git_parity.rs) cover
specific command pairs; they do not promise identical behavior for every Git feature.
