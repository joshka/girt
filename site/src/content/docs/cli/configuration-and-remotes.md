---
title: Configuration and remotes
description: Inspect configuration provenance and remote endpoints.
---

`config get` reads the effective value; `list --show-origin` explains its source. `set` and `unset`
edit local configuration. `remote list`, `add` and `show` manage endpoint configuration;
`remote heads` queries advertised references without fetching. Run `girt help config` and
`girt help remote` for flags.

## Read an effective value

Use the repository from [Comparing with Git](/girt/cli/comparing-with-git/).

```text
$ girt config get core.bare
false
$ git config --get core.bare
false
```

Both report a working-tree repository. The corresponding Git commands are `git config --list`,
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
