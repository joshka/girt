---
title: Fetch, push and clone
description: Transfer objects and references over local paths, HTTP(S) and SSH.
---

:::note[Work in progress]
This guide is an outline. Until it is written, the linked API docs and examples are the best source.
:::

## Outline

- Remotes and refspecs: what girt resolves from configuration and what the caller supplies.
- Fetch: negotiation, receiving and validating packs, installing objects, then updating refs as a
  separate step.
- Clone: fetch into a new repository, with or without checkout.
- Push: choosing commands, the remote's reply, and partial publication.
- Transports: native local paths, smart HTTP(S) (`http` feature) and system OpenSSH (`ssh` feature).
- Credentials, timeouts, cancellation and limits.
- The `workflow` module's configured-remote helpers, used by jj.

## Reference

- [`remote`](https://docs.rs/girt/latest/girt/remote/index.html),
  [`fetch`](https://docs.rs/girt/latest/girt/fetch/index.html),
  [`clone`](https://docs.rs/girt/latest/girt/clone/index.html),
  [`push`](https://docs.rs/girt/latest/girt/push/index.html),
  [`transport`](https://docs.rs/girt/latest/girt/transport/index.html) and
  [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) on docs.rs.
- Examples: [`fetch_remote`](https://github.com/joshka/girt/blob/main/examples/fetch_remote.rs),
  [`clone_repository`](https://github.com/joshka/girt/blob/main/examples/clone_repository.rs),
  [`push_local`](https://github.com/joshka/girt/blob/main/examples/push_local.rs),
  [`http_local`](https://github.com/joshka/girt/blob/main/examples/http_local.rs),
  [`ssh_local`](https://github.com/joshka/girt/blob/main/examples/ssh_local.rs).
- [HTTP setup](https://github.com/joshka/girt/blob/main/docs/http.md) and
  [SSH setup](https://github.com/joshka/girt/blob/main/docs/ssh.md).
