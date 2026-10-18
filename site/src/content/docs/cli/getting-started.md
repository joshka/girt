---
title: Getting started with the CLI
description: Create an example repository and inspect it with girt.
---

Use `girt` to inspect a repository and try the operations exposed by the library. Commands are
grouped by concepts such as objects, references and history. `girt help` lists the groups;
`girt help object` lists the object commands.

:::caution[Experimental CLI]
The girt CLI is not a replacement for Git. It exercises the library's Git operations and provides a
testing surface for performance and byte-for-byte comparisons with Git. Command names and parameters
may change; the library is the core deliverable.
:::

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

## Read a committed file

`tree list` shows the entries in a revision's tree. `object cat` reads an object's payload;
`HEAD:notes.txt` selects the file stored in the latest commit.

<!-- markdownlint-disable MD010 -->

```text
$ girt tree list HEAD
100644 blob 4cb29ea38f70d7c61b2a3a25b02e3bdf44905402	notes.txt
$ girt object cat HEAD:notes.txt
one
two
three
```

<!-- markdownlint-enable MD010 -->

The tree entry identifies a regular file (`100644`), its blob ID and its path. Reading the blob
returns the committed contents, even if you have edited the working file since committing.

## Choose an operation

Follow [Objects and trees](/girt/cli/objects-and-trees/) to inspect stored data,
[History and blame](/girt/cli/history/) to query commits, or
[Index and working tree](/girt/cli/index-and-working-tree/) to inspect files on disk. Each command
page explains its output and links to the library API behind the operation. Git command equivalents
are included as a reference for readers who already know Git.

Use disposable copies for operations that change references, the index, files or storage. The CLI
can perform real writes; read the command's help and limits before using it on a repository you care
about. For testing and benchmarking workflows, see
[Comparing with Git](/girt/cli/comparing-with-git/).
