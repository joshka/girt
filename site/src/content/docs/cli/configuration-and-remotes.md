---
title: Configuration and remotes
description: Inspect configuration provenance and remote endpoints.
---

`config get` reads the effective value; `list --show-origin` explains its source. `set` and `unset`
edit local configuration. `remote list`, `add` and `show` manage endpoint configuration;
`remote heads` queries advertised references without fetching. Run `girt help config` and
`girt help remote` for flags.

:::caution[Experimental CLI]
The girt CLI is not a replacement for Git. It exercises the library's Git operations and provides a
testing surface for performance and byte-for-byte comparisons with Git. Command names and parameters
may change; the library is the core deliverable.
:::

## Read an effective value

Use the repository from [Getting started with the CLI](/girt/cli/getting-started/).

```text
$ girt config get core.bare
false
```

`false` means the repository is not bare. `config get` returns the last effective value after
configuration sources have been combined; use `config list --show-origin` to see where entries came
from.

The corresponding Git commands are `git config --get`, `git config --list`,
`git config --local <key> <value>`, `git config --local --unset-all <key>`, `git remote -v`,
`git remote add` and `git ls-remote`. `remote show` prints configured URLs and refspecs; it does not
perform all the queries of `git remote show`.

## Library API and limits

Reads use
[Repository::config](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.config)
and [Config](https://docs.rs/girt/latest/girt/config/struct.Config.html) (`value` and `entries`).
Writes use
[Repository::edit_config](https://docs.rs/girt/latest/girt/repository/struct.Repository.html#method.edit_config)
and a configuration document. Remote lookup uses
[Remote](https://docs.rs/girt/latest/girt/remote/struct.Remote.html) (`find` and `names`); local,
HTTP and SSH discovery use functions in [fetch](https://docs.rs/girt/latest/girt/fetch/index.html).

An unset key exits 1. Edits affect the local file, while reads include the configured source stack.
HTTP `remote heads` uses anonymous discovery; SSH uses system OpenSSH on macOS/Linux with no
terminal prompts. Trust configured SSH command strings because they execute through the shell. See
[Configuration and provenance](/girt/guides/configuration/) and
[Credentials and transports](/girt/guides/credentials-and-transports/).
