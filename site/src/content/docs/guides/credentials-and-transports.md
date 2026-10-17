---
title: Credentials and transports
description: How girt reaches local paths, HTTP(S) and SSH remotes, and where credentials come from.
---

girt has three transports. Which one a transfer uses depends on the remote's URL after `insteadOf`
rewriting:

| URL                                                | Transport      | Needs                         |
| -------------------------------------------------- | -------------- | ----------------------------- |
| `/srv/repo.git`, `../repo`, `file:///srv/repo.git` | Native local   | nothing                       |
| `https://host/repo.git`, `http://…`                | Smart HTTP     | `http` feature                |
| `ssh://user@host/repo.git`, `user@host:repo.git`   | System OpenSSH | `ssh` feature, macOS or Linux |
| `git://…`, `<helper>::<address>`                   | —              | unsupported                   |

## Native local

A local transfer reads and writes the other repository's object storage directly. It does not start
`git upload-pack` or `git receive-pack` and needs no Git installation. It follows the receiving
repository's relevant settings, such as `receive.denyCurrentBranch`, and runs its receive hooks.
Both SHA-1 and SHA-256 repositories are supported. Local transfers need no async runtime.

## Smart HTTP(S)

The `http` feature adds the smart HTTP protocol over reqwest with rustls. Certificates are checked
against the platform's trust store unless `http.sslCAInfo` or `GIT_SSL_CAINFO` supplies a bundle,
which then replaces the platform roots. `http.sslVerify=false` and `GIT_SSL_NO_VERIFY` are honored.
girt also supports `http.proxy`, `remote.<name>.proxy`, the proxy environment variables (HTTP
proxies with CONNECT only), and same-origin redirects. Settings girt does not implement, such as
client certificates, fail before any network I/O instead of being ignored.

The core HTTP adapters are `async` and run on a Tokio runtime you own. `workflow` creates a private
single-threaded runtime per call instead. Don't call `workflow` from inside a Tokio runtime, since
Tokio panics when a runtime starts inside another; use `spawn_blocking`.

See [HTTP setup](https://github.com/joshka/girt/blob/main/docs/http.md) for the full list of
settings, limits and TLS behavior.

## SSH

The `ssh` feature runs the system OpenSSH client, so authentication, host keys, agents and
`~/.ssh/config` behave as they do for `ssh` itself. girt chooses the command the way Git does, from
`GIT_SSH_COMMAND`, `core.sshCommand` or `GIT_SSH`, but only accepts the OpenSSH variant. Prompts for
key passphrases go through `SSH_ASKPASS`, because the SSH process runs in its own process group
without the terminal, unless the application attaches one explicitly. SSH's standard error, which
explains host-key and authentication failures, is passed to `TransferCallbacks::transport_message`.
Show it to the user when a transfer fails.

See [SSH setup](https://github.com/joshka/girt/blob/main/docs/ssh.md).

## Protocol versions

Fetches over HTTP and SSH ask the server for the upload-pack protocol named by `protocol.version` in
`workflow`: version 2 when it is unset, as Git does. The core adapters ask for version 0 unless
given a `RequestedProtocol`. Whatever was requested, girt accepts the version the server actually
answers with. Version 2 allows a client to ask the server to list only the references it needs; girt
does not use that yet, so a fetch still lists every reference. Push always uses the version 0
receive-pack protocol, as Git does.

## Credentials

Only HTTP(S) uses girt's credential handling. SSH credentials belong to OpenSSH.

When an HTTP server answers `401 Unauthorized`, `workflow` asks for a credential once and retries:

1. Helpers from `credential.helper` are asked in order, as Git asks them. `store` is answered
   natively by `CredentialStore`, which reads and writes the same `~/.git-credentials` files with
   the same locking. Any other name runs the `git-credential-<name>` program found on `PATH` or in
   Git's exec path, so helpers such as `osxkeychain` and `manager` keep working.
2. If no helper supplies a complete credential, `TransferCallbacks::credential` asks the user for
   the username, then the password. Return `None` to cancel.
3. When the server accepts the credential, every helper is told to store it. When the server rejects
   it, every helper is told to erase it.

With the core APIs, the application runs this lifecycle itself with `CredentialSession::fill`,
`approve` and `reject`. Configured helper names are never executed automatically. A repository's
configuration can come from an untrusted clone, so the application maps each helper name to a
program or to the native store it approves. A helper with no mapping fails before anything runs.

Credentials are not written to logs or `Debug` output. Errors name the remote URL without its
username or password. Plain `http://` sends credentials unencrypted.

## Reference

- [`transport`](https://docs.rs/girt/latest/girt/transport/index.html),
  [`remote`](https://docs.rs/girt/latest/girt/remote/index.html) (credentials and URLs) and
  [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) on docs.rs.
- [Fetch, push and clone](/girt/guides/fetch-push-clone/) and
  [Limits and trust](/girt/guides/limits-and-trust/).
