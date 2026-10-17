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

The `workflow` module reads the settings and environment variables described below for you. The core
transport APIs read nothing themselves: the application passes in each setting, so it decides which
ones to honor.

## Native local

A local transfer reads and writes the other repository's object storage directly. It does not start
`git upload-pack` or `git receive-pack`, and needs no Git installation and no async runtime. Both
SHA-1 and SHA-256 repositories are supported.

A push to a local path follows the destination's `receive.*` settings, such as
`receive.denyCurrentBranch`. On macOS and Linux it runs the destination's `pre-receive`, `update`,
`post-receive` and `post-update` hooks, honoring `core.hooksPath`. Installed `proc-receive`,
`reference-transaction` or `push-to-checkout` hooks are unsupported, and so are receive hooks on
other platforms: a push that would need them is refused before any reference changes. See
[`push::send_local`](https://docs.rs/girt/latest/girt/push/fn.send_local.html) for the full
contract.

## Smart HTTP(S)

The `http` feature adds the smart HTTP protocol over reqwest with rustls. Through `workflow`:

- Certificates are checked against the platform's trust store. `http.sslCAInfo` or `GIT_SSL_CAINFO`
  names a bundle that replaces the platform roots.
- `http.sslVerify=false` and `GIT_SSL_NO_VERIFY` disable verification, as in Git.
- `http.proxy`, `remote.<name>.proxy`, `https_proxy`, `http_proxy`, `all_proxy` and `no_proxy`
  select a proxy. Only HTTP proxies, using CONNECT, are supported.
- A redirect on the first request is followed if it stays on the same origin.
- Settings girt does not implement, such as client certificates, fail before any network I/O instead
  of being ignored.

With the core API, the application resolves `HttpSettings` from the configuration and an
`HttpEnvironment` it fills in itself: the CA bundle's bytes (girt never opens the file), the proxy
it selected, and `allow_insecure_tls`, without which disabling verification is refused. Then it
builds an `HttpRemote`. `HttpRemote::new` is the alternative for applications that supply their own
`Authorization` header and want no Git configuration at all.

The core HTTP adapters are `async` and run on a Tokio runtime you own. `workflow` creates a private
single-threaded runtime per call instead. Don't call `workflow` from inside a Tokio runtime, since
Tokio panics when a runtime starts inside another; use `spawn_blocking`.

See [HTTP setup](https://github.com/joshka/girt/blob/main/docs/http.md) for the full list of
settings, limits and TLS behavior.

## SSH

The `ssh` feature runs the system OpenSSH client, so authentication, host keys, agents and
`~/.ssh/config` behave as they do for `ssh` itself. girt never sees a key or password. `workflow`
chooses the command the way Git does, from `GIT_SSH_COMMAND`, `core.sshCommand` or `GIT_SSH`, and
runs it through `/bin/sh` as Git does. Only the OpenSSH variant is accepted.

With the core API, `SshRemote::openssh` keeps the user's OpenSSH configuration, and the application
must approve any configured command by giving its executable and arguments explicitly
(`ApprovedSshCommand`); girt never runs it through a shell. `SshRemote::configured` and
`SshRemote::new` instead pin an explicit OpenSSH configuration file and force restrictive options,
for services that must not depend on a user's settings.

The SSH process runs in its own process group without the terminal, unless the application attaches
one. Passphrase prompts therefore need an agent or `SSH_ASKPASS`. SSH's standard error, which
explains host-key and authentication failures, is passed to `TransferCallbacks::transport_message`;
show it to the user when a transfer fails.

See [SSH setup](https://github.com/joshka/girt/blob/main/docs/ssh.md).

## Protocol versions

Fetches over HTTP and SSH ask the server for an upload-pack protocol version. `workflow` asks for
the version in `protocol.version`, or version 2 when it is unset, as Git does. The core adapters ask
for version 0 unless given a `RequestedProtocol`. A server asked for version 2 may answer with
version 0 or 1, and girt accepts that. A server that answers with version 2 when it was not asked
for it is refused. Version 2 allows a client to ask for only the references it needs; girt does not
use that yet, so a fetch still lists every reference. Push uses the version 0 or 1 receive-pack
protocol, which has no version 2.

## Credentials

Only HTTP(S) uses girt's credential handling. SSH credentials belong to OpenSSH.

When an HTTP server answers `401 Unauthorized` and offers Basic authentication, `workflow` asks for
a credential once and retries:

1. Helpers from `credential.helper` are asked in order, as Git asks them. `store`, optionally with
   `--file <path>`, is answered natively by `CredentialStore`, which reads and writes the same
   `~/.git-credentials` files with the same locking. A value starting with `!` runs as a shell
   command. An absolute path runs that program. Any other name runs the `git-credential-<name>`
   program found on `PATH` or in Git's exec path, so helpers such as `osxkeychain` and `manager`
   keep working.
2. If no helper supplies a complete credential, `TransferCallbacks::credential` asks the user for
   the username, then the password. Return `None` to cancel.
3. When the server accepts the credential, every helper is told to store it. When the server rejects
   it, every helper is told to erase it.

Because `workflow` runs the helpers a repository's configuration names, it trusts that configuration
as Git does. Use it only for repositories whose configuration you trust.

With the core API, the application decides which helpers may run: it maps each configured
`credential.helper` value to a program or to the native store, and `CredentialSession::fill` refuses
any helper without a mapping before running anything. Pass the filled session to
`HttpRemote::with_credentials`. The HTTP adapter then sends the credential only after a `401`
challenge offering Basic authentication, only to the same origin, and reports the server's answer to
the helpers itself.

Credentials are not written to logs or `Debug` output. Errors name the remote URL without its
username or password. Plain `http://` sends credentials unencrypted.

## Reference

- [`transport`](https://docs.rs/girt/latest/girt/transport/index.html),
  [`remote`](https://docs.rs/girt/latest/girt/remote/index.html) (credentials and URLs) and
  [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) on docs.rs.
- [Fetch, push and clone](/girt/guides/fetch-push-clone/) and
  [Limits and trust](/girt/guides/limits-and-trust/).

## CLI transfers

The `girt` CLI supports SSH for `clone`, `remote heads`, `fetch` and `push` on macOS/Linux. Use
either URL form from the table above. For example:

```sh
girt remote heads git@example.com:team/project.git
girt clone ssh://git@example.com/team/project.git
girt -C project fetch origin
girt -C project push origin main
```

OpenSSH reads its normal configuration, identities, agent and host-trust files. The CLI selects
`GIT_SSH_COMMAND`, then `core.sshCommand`, then `GIT_SSH`, then `ssh` on PATH. Command strings
execute through the shell; only use configuration you trust. Clone reads system/global configuration
and ignores the enclosing repository. Discovery also reads global configuration when run outside a
repository. Terminal prompts and job control are unavailable; use an agent or askpass. Non-OpenSSH
variants, percent encoding, query/fragment fields and repository paths starting with `~` remain
unsupported.

HTTP(S) clone and discovery currently allow anonymous access only. Fetch and push use configured
credential helpers through `workflow`, but do not prompt interactively for missing credentials.
Clone checks out the default branch unless `--bare` is used.
