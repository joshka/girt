# SSH Fetch and Push

Enable `ssh` on macOS/Linux for protocol v0 over a system OpenSSH client. Supply
`transport::ssh::SshRemote` with literal host, user, port and repository path components, an
absolute OpenSSH executable path and an absolute configuration file path. Call `fetch::receive_ssh`
or `push::send_ssh` inside a caller-owned Tokio runtime with I/O and time enabled.

## Endpoints and Configuration

Hosts accept DNS names, IPv4 and bare IPv6 addresses. Usernames accept ASCII letters, digits, dots,
underscores and hyphens, without a leading hyphen. Ports must be nonzero. Repository paths are
literal UTF-8, absolute or relative to the remote account's working directory. Paths may contain
spaces, quotes and shell metacharacters. Empty paths, leading hyphens/tilde and control characters
are rejected. Host/user components are limited to 255 bytes and paths/configuration filenames to
8192 bytes. URLs, scp-style endpoints, percent decoding and home expansion are not implemented;
callers parse their own supported configuration into these components.

The remote command is exactly `git-upload-pack 'quoted path'` or `git-receive-pack 'quoted path'`.
Embedded single quotes are escaped using POSIX shell quoting. Neither service names nor arbitrary
SSH arguments are caller inputs. Host, user and port are separate executable arguments, with `--`
before the host. The remote account must provide a compatible shell and Git services. A server may
restrict these through its own forced command.

The explicit `-F` file can select identity files, host aliases, known-host files and algorithm
policy. Use `/dev/null` to select no config. An illustrative caller-owned config is:

```sshconfig
IdentityFile /absolute/path/to/dedicated_key
UserKnownHostsFile /absolute/path/to/known_hosts
GlobalKnownHostsFile /dev/null
```

Pre-provision host trust through a trusted channel. Enforced command-line options require strict
host-key checking and batch mode, disable host-key updates, and prevent
password/keyboard-interactive authentication, agents, default identity filenames, forwarding,
proxies, connection sharing, TTYs, local commands and backgrounding. Caller config cannot override
these settings. Only explicitly selected public-key identities are used. Encrypted keys needing a
prompt fail; hardware keys selected by the caller may still need user presence, so impose a
deadline. The environment is cleared except PATH and an explicit askpass prohibition; `GIT_SSH*`,
`SSH_AUTH_SOCK`, askpass and `GIT_PROTOCOL` environment discovery are not used. Explicit config
`SetEnv`/`SendEnv` directives remain caller policy; do not request another Git protocol version.
Unsupported service versions are rejected before update commands. Connection attempts are limited to
one.

Trust the executable and config. OpenSSH includes and `Match exec` can run local programs; this
boundary is not a configuration sandbox. OpenSSH's own parsing, algorithm negotiation and platform
behavior remain external requirements. Unsupported OpenSSH options fail at client startup; tested
runtime evidence uses OpenSSH 10.3. There is no keychain integration, credential helper, password
storage, automatic authentication retry or cryptographic implementation in girt.

## Runtime, Bounds and Cleanup

SSH service I/O is asynchronous and uses nonblocking pipes registered with Tokio. One owner reads
advertisements, writes requests while concurrently draining responses, disposes of stderr, and
observes process exit. Full pipes in either direction do not require a blocking worker. Ready loops
yield so cancellation and other runtime work can progress. No runtime or detached task is created.
The `ssh` feature does not enable HTTP/TLS dependencies or async filesystem operations.

`receive_ssh` takes `Option<Arc<KnownHistory>>` and returns an owned `DownloadedFetch`. Pass `None`
for a full transfer without preparing or allocating history. With `Some`, the result privately
retains the exact negotiation history without copying objects. It is `Send + Sync + 'static`: move
the download into a caller-managed bounded blocking worker after the initiating scope drops its Arc.
Validation consumes the download and releases its history ownership on success or failure; dropping
the download also releases it. Other Arc owners may retain history independently. Installation still
rechecks dependencies and requires caller coordination with GC.

Validation is a separate synchronous step; it decodes/indexes packs, checks identities and proves
connectivity before explicit installation. Prepare history and push packs outside the executor or on
a caller-owned bounded worker. Bound waiting downloads and aggregate retained bytes as well as
active workers. The [runnable example](../examples/ssh_local.rs) submits one worker request, holds
its permit through validation, observes completion explicitly and installs outside the executor.
Dropping a started worker's handle does not stop it; retain completion ownership after requesting
cancellation. `send_ssh` borrows `PreparedPush` without copying its buffers. Network futures are
`Send` when selection callbacks are `Send`; borrowed inputs must live through the operation.
Advertisement parsing, request encoding and status parsing are synchronous work bounded by the
configured counts/bytes. Callbacks must return promptly. There is no executor latency promise for
arbitrarily large limits or callbacks.

Retained fetch protocol bytes are capped by `max_wire_bytes`, including the advertisement. Push
advertisements and responses use `max_advertisement_bytes` and `max_status_bytes`. Existing pack,
object, graph and decode budgets apply during preparation/validation. Pipe buffers, OpenSSH memory,
allocator overhead and server resources are separate. Each concurrent call has its own limits;
callers bound concurrency and aggregate memory.

`TransportControl` checks cancellation at most every 20 ms during waits, subject to scheduling. Its
absolute deadline spans SSH handshake, service I/O, diagnostic disposal and exit, without extending
on traffic. With no deadline, a stalled peer waits until cancelled. Spawn/path lookup and kernel
reaping are synchronous and can exceed that interval; hashing, validation and filesystem work have
separate caller-owned lifetimes. Calling network APIs requires an active Tokio I/O/time context.

Every SSH child starts in a new local process group. Completion, failure, dropped futures and
unwinding kill that group before reaping its leader. Callers must not install a handler that reaps
these children. Descendants escaping the group and elevated processes are outside the guarantee.
Remote services are not members of the local group: terminating SSH cannot roll back refs or promise
that a remote hook has stopped. Stderr is continuously drained and discarded, including during
upload and exit waits, so an undrained diagnostic sink cannot block an operation.

## Results and Recovery

Errors expose static categories, I/O kinds and unsuccessful exit codes without command, endpoint,
key/config paths or stderr. OpenSSH exit 255 cannot reliably distinguish host trust, authentication
and network failure, and a remote command can itself exit 255. Check the explicitly supplied
configuration when diagnosing these failures. Git progress and rejection text remain
server-controlled bytes; callers decide how to display them.

A push that fails before attempting command bytes is `NotSent`. After an attempted write, failure is
`Uncertain`; inspect remote refs before retrying. Valid status packets already received are
retained, even when a complete report is followed by stalled EOF, failed exit or cancellation.
Complete Git rejection and mixed success return `Ok` reports whose individual statuses must be
checked. Dropping a polled future performs local cleanup but cannot return that evidence; prefer
setting cancellation and awaiting the result. Fetch failure produces no installable result or local
mutation.

## Disposable Fixture and Validation

With Git, Python 3, OpenSSH, ssh-keygen and a usable local sshd installed:

```sh
cargo run --features ssh --example ssh_local
cargo test --features ssh --test ssh
cargo bench --features ssh --bench ssh
```

The original Python fixture binds sshd to IPv4 loopback on an unprivileged ephemeral port. It uses
temporary host/client keys, explicit authorized keys, config and known_hosts, authenticates only the
current OS user, and restricts execution to the selected disposable repository and two Git services.
It disables password and keyboard-interactive authentication and forwarding. A forced command
validates service/path arguments before executing the original quoted command through a POSIX shell;
this exercises the actual quoting. It does not change `~/.ssh`, account settings or system sshd
configuration. Fault cases have finite watchdogs. Fixture teardown terminates its owned process tree
and deletes temporary secrets.

The native fixture worked without elevation on macOS arm64. Some systems restrict unprivileged sshd;
startup diagnostics are fixture errors, never skipped interoperability evidence. Run in a disposable
container with an isolated test account when the host disallows it. Do not enable a persistent
system SSH service or relax production trust/authentication to run tests.

The tests establish real Git full/incremental/known-only fetch, initial/subsequent/no-op push,
branches/tags, deltas and history exclusion, literal path quoting, unknown/changed host rejection,
authentication failure, mixed ref rejection and cancellation after a real ref update. Original fault
services cover malformed framing, wire limits, diagnostics floods, stalled
handshake/service/upload/response/exit and cancellation with retained statuses. Process tests
separately prove local reaping, future-drop cleanup and group isolation. Fault services are not
claimed as Git interoperability. See [compatibility evidence](compatibility.md#ssh-transport) and
[benchmark evidence](benchmarks.md#ssh-loopback-baseline).

## Dependencies and Deferred Work

No new crate or version requirement is added. `ssh` enables the existing optional Tokio dependency
(MIT), including its `net` feature for `AsyncFd`; existing rustix (Apache-2.0 OR MIT) supplies Unix
process and descriptor operations. Core-only builds remain runtime-free. OpenSSH is an external
caller-selected executable with its own distribution notices, not linked or vendored code.

Supported Git scope stays SHA-1 protocol v0 and non-thin packs. Windows SSH, proxy/jump hosts,
connection reuse, URL/refspec/remote policy, protocol v2, shallow/partial repositories and new
credential services remain excluded. Broader async object-store/filesystem interfaces and storage
concurrency need a subsequent design investigation driven by a consumer's responsiveness
requirements; this adapter does not decide them.
