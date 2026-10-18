---
title: Fetch, push and clone
description: Exercise the library transfer workflows on disposable repositories.
---

`clone` creates a repository and normally checks out HEAD. `fetch` updates configured refspecs;
`push` publishes explicit refspecs with a lease based on remote-tracking state. Use disposable
repositories to experiment with writes. Run `girt help clone`, `girt help fetch` and
`girt help push` for effects and options.

:::caution[Experimental CLI]
The girt CLI is not a replacement for Git. It exercises the library's Git operations and provides a
testing surface for performance and byte-for-byte comparisons with Git. Command names and parameters
may change; the library is the core deliverable.
:::

## Clone locally

Use the repository from [Getting started with the CLI](/girt/cli/getting-started/).

Clone the current repository into a new sibling directory. The source directory in the absolute
destination path is shown as `/path/to/cli-example`. The result goes to stdout; live progress goes
to stderr when it is a terminal.

```text
$ girt clone . ../cli-example-clone
cloned into /path/to/cli-example/../cli-example-clone
HEAD -> refs/heads/main e5d83fbd011e44793ba5f60d5091bb569e12352f
```

The result names the destination and the branch and commit selected for HEAD. The clone includes a
working tree unless you pass `--bare` or `--no-checkout`.

`girt clone` corresponds to `git clone`; `girt fetch origin` corresponds to `git fetch origin` for
the configured refspecs. `girt push origin main:main` corresponds to
`git push --force-with-lease origin main:main` for the CLI's lease check, though a non-fast-forward
update also needs girt's `+` refspec prefix. Fetch first if the lease rejects stale state.

## Library API and limits

The commands use
[workflow::clone_with](https://docs.rs/girt/latest/girt/workflow/fn.clone_with.html),
[workflow::fetch](https://docs.rs/girt/latest/girt/workflow/fn.fetch.html) and
[workflow::push](https://docs.rs/girt/latest/girt/workflow/fn.push.html) with an explicit
environment and options. These workflows compose discovery, transfer, publication and checkout; Rust
callers can inspect structured outcomes and partial-failure information.

Local paths, HTTP(S) credential helpers and system OpenSSH on macOS/Linux are supported. Interactive
credential and SSH terminal prompts are unavailable. Fetch does not automatically follow tags.
`--git` suppresses progress but keeps girt's result format; byte-for-byte output comparison with Git
is not available for these commands. See [Fetch, push and clone](/girt/guides/fetch-push-clone/) for
publication and recovery boundaries.
