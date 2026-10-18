---
title: Fetch, push and clone
description: Exercise the library transfer workflows on disposable repositories.
---

`clone` creates a repository and normally checks out HEAD. `fetch` updates configured refspecs;
`push` publishes explicit refspecs with a lease based on remote-tracking state. Use disposable
repositories to experiment with writes. Run `girt help clone`, `girt help fetch` and
`girt help push` for effects and options.

## Clone locally

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

These commands make separate destinations from the same source. The source directory in girt's
absolute destination path is shown as `/path/to/cli-example`. Git writes its progress below to
stderr; girt writes its result to stdout.

```text
$ girt clone . ../cli-example-clone
cloned into /path/to/cli-example/../cli-example-clone
HEAD -> refs/heads/main e5d83fbd011e44793ba5f60d5091bb569e12352f
$ git clone . ../cli-example-git-clone
Cloning into '../cli-example-git-clone'...
done.
```

The cloned HEAD resolves to the same commit. `girt fetch origin` corresponds to `git fetch origin`
for the configured refspecs. `girt push origin main:main` corresponds to
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
