# SSH Fetch and Push

Enable `ssh` on macOS/Linux for protocol v0 over a system OpenSSH client. Supply
`transport::ssh::SshRemote` with literal host, user, port and repository path components, an
absolute OpenSSH executable path and an absolute configuration file path. Call `fetch::receive_ssh`
or `push::send_ssh` inside a caller-owned Tokio runtime with I/O and time enabled.

`SshRemote::configured` accepts a resolved SSH destination and `SshEnvironment` supplied by the
application. It supports `ssh://[user@]host[:port]/path` and `[user@]host:path`. The application
supplies a default user and trusted executable and config paths. Percent encoding, URL query or
fragment fields, implicit tilde expansion and non-OpenSSH variants are refused. Local paths, file
URLs, and helper schemes use separate transport paths; SSH has no Git-executable fallback.

`SshRemote::openssh` instead keeps ordinary OpenSSH configuration policy with explicit executable
approval and a complete caller-supplied environment. It preserves absent URL user and port values.
The session uses pipes and a separate process group. An explicit foreground terminal attachment
supports local prompts with caller-owned job control. See
[ordinary OpenSSH policy](#ordinary-openssh-policy) before admitting a consumer configuration.

## Endpoints and Configuration

Hosts accept DNS names, IPv4 and bare IPv6 addresses. Usernames accept ASCII letters, digits, dots,
underscores and hyphens, without a leading hyphen. Ports must be nonzero. Repository paths are
literal UTF-8, absolute or relative to the remote account's working directory. Paths may contain
spaces, quotes and shell metacharacters. Empty paths, leading hyphens/tilde and control characters
are rejected. Host/user components are limited to 255 bytes and paths/configuration filenames to
8192 bytes. The explicit constructor accepts components; the configured constructor parses the
limited URL and scp forms above. Other endpoint forms remain unsupported.

The remote command is exactly `git-upload-pack 'quoted path'` or `git-receive-pack 'quoted path'`.
Embedded single quotes are escaped using POSIX shell quoting. The service name is fixed; configured
SSH arguments require application approval. In the restrictive constructors, host, user and port are
separate arguments, with `--` before the host. The remote account must provide a compatible shell
and Git services. A server may restrict these through its own forced command.

The explicit `-F` file can select identity files, host aliases, known-host files and algorithm
policy. Use `/dev/null` to select no config. An illustrative caller-owned config is:

```sshconfig
IdentityFile /absolute/path/to/dedicated_key
UserKnownHostsFile /absolute/path/to/known_hosts
GlobalKnownHostsFile /dev/null
```

Pre-provision host trust through a trusted channel. Enforced command-line options require strict
host-key checking, disable host-key updates, and prevent password/keyboard-interactive
authentication, default identity filenames, forwarding, proxies, connection sharing, TTYs, local
commands and backgrounding. Batch mode applies unless askpass is selected. Caller config cannot
override these settings. Only explicitly selected public-key identities are used. The explicit
constructor disables agents and prompts; hardware keys may still need user presence, so impose a
deadline. The environment is cleared except the selected PATH and authentication variables.
`GIT_PROTOCOL` environment discovery is not used. Explicit config `SetEnv`/`SendEnv` directives
remain caller policy; do not request another Git protocol version. Unsupported service versions are
rejected before update commands. Connection attempts are limited to one.

The configured constructor selects `GIT_SSH_COMMAND`, then the last `core.sshCommand`, then
`GIT_SSH`. These are application-supplied values, never read from the process environment. A
selected value must exactly match `ApprovedSshCommand::configured`; the application supplies the
absolute executable and literal argument vector after parsing and approving it. Girt never executes
configured shell snippets. `GIT_SSH` denotes an executable path; `GIT_SSH_COMMAND` and
`core.sshCommand` can contain command arguments in Git. The approval mapping must preserve that
distinction. Empty commands and selected config keys without values fail before spawning; they never
select a lower-priority command. Environment overrides bypass even valueless config keys. Approved
arguments are trusted application policy and can affect OpenSSH behavior.

`GIT_SSH_VARIANT` overrides the last `ssh.variant`; only the case-insensitive `ssh` variant is
accepted. An absent variant means the caller has selected OpenSSH. Girt does not run Git's automatic
`-G` probe, infer PuTTY variants from executable names, or treat unknown variant values as `ssh`.
Explicit `auto`, `simple`, PuTTY variants, and empty or unknown values remain unsupported. An
unapproved command or variant fails before process creation. The application may supply an agent
socket and an askpass executable. Askpass enables encrypted-key passphrase prompts while password
and keyboard-interactive authentication remain disabled. Girt supplies `SSH_AUTH_SOCK` and
`SSH_ASKPASS` only when selected; neither is discovered globally.

Trust the executable and config. OpenSSH includes and `Match exec` can run local programs; this
boundary is not a configuration sandbox. OpenSSH's own parsing, algorithm negotiation and platform
behavior remain external requirements. Unsupported OpenSSH options fail at client startup; tested
runtime evidence uses OpenSSH 10.3. There is no keychain integration, credential helper, password
storage, automatic authentication retry or cryptographic implementation in girt.

## Ordinary OpenSSH Policy

`SshRemote::openssh(config, destination, OpenSshOptions)` supports the same limited URL and scp
syntax. `OpenSshOptions` supplies an absolute default executable, an optional exact command
approval, and the complete child environment. Its `GIT_SSH_COMMAND`, `GIT_SSH`, and
`GIT_SSH_VARIANT` entries participate in the same command and variant precedence described above.
The caller asserts OpenSSH semantics for the executable; girt does not infer variants, probe
commands, or parse shell syntax. An unapproved or mismatched selected command fails before spawning.
Environment names and values are checked for invalid process-environment bytes; Debug and errors
omit them.

The ordinary command adds only approved literal arguments, `-p` when the URL supplies a port,
`[user@]host`, and the quoted fixed service/path. It supplies no `-F`, default user/port, or
restrictive `-o` flags. OpenSSH can resolve aliases, users, ports, identities, agents, proxies,
connection sharing, authentication, host verification and host-key updates through its normal
configuration. Applications must trust those files and any local commands they enable. Approval of
an executable does not establish compatibility with all of its configuration.

The environment is replaced by exactly the caller's map. Applications can explicitly capture
inherited variables or construct a narrower map; girt does not discover `HOME`, `PATH`, agents,
askpass, display settings or credentials. `GIT_PROTOCOL` must be absent or `version=0`. Girt adds no
`SendEnv` option; caller-owned `SetEnv`/`SendEnv` or wrappers must not request another version. Only
a protocol v0 advertisement is supported. Production girt starts the approved SSH executable, never
a local Git executable; Git services remain on the server.

Without a terminal attachment, the new process group can make `/dev/tty` prompts inaccessible;
askpass can operate through the supplied environment. Applications can opt into raw local stderr and
[foreground terminal leasing](#foreground-terminal-leasing). Consumer integrations still own job
control and display policy. A launched failure is terminal for that attempt: authentication, trust,
proxy or local-command effects may already have occurred; never automatically retry through another
transport.

Original recording executables compare ordinary argv with the installed Git executable under
protocol v0, including omitted and explicit user/port, scp syntax and IPv6. Disposable OpenSSH tests
verify config-selected user, port, identity and trust, rejection of unknown/changed keys, and the
existing cancellation/reaping boundary. These are bounded compatibility observations rather than
proof of ordinary interactive SSH parity.

## Foreground Terminal Leasing

`ForegroundTerminal::prepare(OwnedFd)` returns a non-cloneable attachment and `TerminalEvents`. The
descriptor must be read/write and identify the caller's foreground controlling terminal with
`TOSTOP` disabled. Preparation only validates; it does not change the terminal or launch a process.
Wrap the attachment in `Arc` and pass it to `SshRemote::with_terminal`. Sequential remotes may share
it; concurrent terminal sessions in the same process are rejected. Restricted SSH constructors
cannot attach a terminal. `SshRemote::has_terminal` reports the selected mode.

The caller must exclusively coordinate terminal I/O, job control and child reaping, including other
threads and libraries. Every launch rechecks foreground ownership and snapshots the full terminal
attributes and caller process group. Before executing SSH, the child establishes its owned process
group and takes the terminal foreground while temporarily blocking SIGTTOU on its thread. The
parent's restoration guard is armed before spawn. Protocol stdin/stdout remain pipes; `/dev/tty`
prompts use the controlling terminal. OpenSSH authentication, host-key, environment and command
selection policies are unchanged.

The caller polls `TerminalEvents::try_next_stop` independently of blocked display work. On a child
stop, girt stops its owned group and restores caller attributes and foreground before publishing a
single bounded `StoppedTerminal` permit. The permit reports the stopping signal. The caller decides
how to suspend its own job and wait for a shell foreground resume. `permit.resume()` grants
permission only after checking caller foreground ownership; the transport rechecks before restoring
SSH attributes, transferring foreground and continuing the child group. A background resume is
rejected. Girt does not install a process-global signal handler or suspend the caller itself.

Dropping a permit or its event receiver cancels the active operation. Cancellation does not wait for
a held permit. Old permits cannot affect a later session. `try_next_stop` returns `None` between
sessions; the operation future and caller cancellation control own completion and coordinator
teardown. Cancellation and explicit absolute deadlines continue while suspended. Girt adds no
default interactive timeout; applications must choose prompt deadlines explicitly.

Cleanup kills the owned group, restores the terminal and reaps the reserved leader before releasing
the lease. Stops and abnormal cleanup discard queued terminal input so unfinished password input
cannot become caller input. Clean exit preserves type-ahead only when echo and caller-compatible
input modes have been restored; a clean exit leaving private input mode also discards input. Input
flushing is immediate and does not wait for terminal output to drain. Once a stop has returned the
terminal to the caller, cancelled or rejected resume leaves a shell or another job's terminal state
alone. Partial resume failures re-arm restoration before the first mutation.

Restoration depends on a live terminal and OS permission; a hung-up descriptor or abrupt termination
of the caller cannot guarantee it. Descendants that escape the process group remain excluded.
Display observers must return promptly; an interactive consumer should use bounded, nonblocking
forwarding and explicit cancellation on overflow so display backpressure cannot prevent stop
observation. Push cancellation preserves received acknowledgement evidence and never authorizes an
automatic retry.

## Runtime, Bounds and Cleanup

SSH service I/O is asynchronous and uses nonblocking pipes registered with Tokio. One owner reads
advertisements, writes requests while concurrently draining responses, disposes of stderr, and
observes process exit. Full pipes in either direction do not require a blocking worker. Ready loops
yield so cancellation and other runtime work can progress. No runtime or detached task is created.
The `ssh` feature does not enable HTTP/TLS dependencies or async filesystem operations.

`fetch::discover_ssh_with_diagnostics` optionally delivers raw local stderr while discovery runs.
Its synchronous callback receives chunks of at most 8192 bytes, including incomplete lines and
non-UTF-8 bytes. Successful completion drains final diagnostics before returning. Cancellation or
transport failure may leave unread bytes. Girt keeps no diagnostic transcript; callers own display,
redaction and retention bounds. These bytes can contain secrets and untrusted terminal escapes and
are separate from remote Git sideband messages. The ordinary `discover_ssh` API discards them, and
errors, Debug and tracing remain redacted.

Diagnostic callbacks must return promptly. They have no result that can request retry or change a
transport outcome; a display failure should be handled by the caller. A callback panic unwinds and
drops the session, preserving process cleanup. Delivery does not change terminal foreground
ownership, authentication, command/environment selection or the no-retry policy. In particular,
streaming stderr does not make a background process group able to read `/dev/tty`.

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
that a remote hook has stopped. Stderr is continuously drained during upload and exit waits and is
discarded unless the caller explicitly supplies a diagnostic observer. Observers must return
promptly so they do not block service I/O or cancellation.

## Live Output and Checked Pushes

`FetchRequest::receive_ssh_with_progress` accepts separate local-diagnostic and remote-notice
callbacks while retaining the existing publication plan and owned `FetchDownload`. The lower-level
`fetch::receive_ssh_with_progress` takes `FetchOptions` for wire limits and optional depth. Local
stderr follows the discovery diagnostic contract above. Remote notices are complete channel-2
payloads, delivered once in wire order after validating the negotiation prefix, including shallow
boundaries when requested. They borrow the retained wire response without adding a transcript.
Validation remains authoritative and replays remote notices; pass a no-op notice callback to
`validate_with_progress` when they were already displayed. Local object/delta validation progress
remains a separate callback on the caller's synchronous worker.

`push::send_ssh_checked_with_progress` borrows `PreparedPush` and invokes a predicate with validated
live advertised tips before writing update commands. A false predicate sends only `0000`, closes
stdin, and awaits the same SSH session's exit. Successful cleanup returns
`SshPushOutcome::Declined`; cleanup failure returns `NotSent`. Neither authorizes automatic
fallback: authentication, host-key, proxy or local-command effects may already have occurred. A true
predicate retains the prepared commands' expected old values for the server to enforce after the
advertisement becomes stale.

`push::send_ssh_selected_with_progress` lets the callback choose a subset of prepared destination
names in the same session. Names must be unique and belong to the prepared command list; submitted
commands retain their original order and exact old/new IDs. Format validation precedes selection,
and the submitted subset determines required report-status, delete-refs and push-options
capabilities. A nondeletion subset borrows the prepared pack unchanged, potentially including
objects for omitted commands, and retains that pack's receiver-root requirements. Deletion-only
selection sends no pack. Empty selection sends only a flush, omits push options, awaits clean exit
and returns `Sent` with an empty report; this API never returns `Declined`. Successful and uncertain
reports contain only submitted refs. Omitted refs are caller observations, not receiver-confirmed
updates. Invalid selection and cleanup failures before update transmission are `NotSent` and do not
authorize automatic retry or fallback.

Call `PreparedPush::with_progress` to request push sideband. When negotiated, the remote observer
receives complete channel-2 payloads before the final report. Local stderr remains a separate
observer. Partial channel-2 packets, bytes outside the response budget, and packets after malformed
or terminal framing are not delivered. The authoritative parser still determines success or
uncertainty, and the bounded report retains complete notices and valid acknowledgement prefixes.
Display callbacks have no result that requests retry or changes publication classification.

Existing SSH receive/send functions supply no-op observers and retain their result types. All
callbacks run synchronously on the caller's async task, must return promptly, and receive untrusted
bytes. Local diagnostics can contain secrets; remote notices never prove successful publication. No
callback changes terminal ownership, authentication policy or automatic-retry behavior.

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
separately prove local reaping, future-drop cleanup and group isolation. Original disposable-PTY
fixtures exercise foreground handoff, prompt input, full attribute restoration, stops during each
I/O phase, resume permission, cancellation, input flushing and clean type-ahead. They re-execute the
test in a new session and never use the invoking user's terminal; current native evidence is macOS
arm64. Fault services are not claimed as Git interoperability. See
[compatibility evidence](compatibility.md#ssh-transport) and
[benchmark evidence](benchmarks.md#ssh-loopback-baseline).

Original recording-wrapper tests in `tests/support/ssh_configuration.rs` compare command selection
and endpoint/service arguments against Git 2.55.0 on macOS arm64, without a network connection or
upstream implementation/test input. The tests use protocol v0 to match girt. Command selection
follows the [Git environment contract](https://git-scm.com/docs/git#Documentation/git.txt-GITSSH)
and
[configuration contract](https://git-scm.com/docs/git-config#Documentation/git-config.txt-coresshCommand).
These comparisons do not establish transparent SSH policy parity: girt still supplies its explicit
`-F` file, user and port, clears the environment, and enforces the authentication and trust options
above. Callers must retain a fallback for ordinary SSH configuration outside that policy.

Configured-path tests cover an SSH URL, command precedence and refusal, a disposable agent and
encrypted-key askpass authentication, and redacted errors. The existing cancellation, exit, cleanup
and uncertain-push tests use the same `Session` owner as configured connections. Native Windows SSH
remains unsupported; girt never spawns a Windows SSH process. Reopen it for a required Windows
consumer endpoint with native quoting, process containment, console and cancellation fixtures.

## Dependencies and Deferred Work

`ssh` enables the existing optional Tokio dependency (MIT), including its `net` feature for
`AsyncFd`. Existing rustix (Apache-2.0 OR MIT) supplies Unix process, descriptor and terminal
operations. The optional `libc = "0.2"` dependency (MIT OR Apache-2.0) supplies portable
thread-local signal-mask and pre-exec handoff calls on macOS/Linux. Core-only builds remain
runtime-free. OpenSSH is an external caller-selected executable with its own distribution notices,
not linked or vendored code.

Supported Git scope uses SHA-1/SHA-256 protocol v0 and non-thin push packs. Windows SSH, proxy/jump
hosts, connection reuse, broader URL/refspec/remote policy, protocol v2, shallow/partial
repositories and new credential services remain excluded. Broader async object-store/filesystem
interfaces and storage concurrency need a subsequent design investigation driven by a consumer's
responsiveness requirements; this adapter does not decide them.
