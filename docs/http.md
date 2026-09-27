# Smart HTTP Fetch and Push

Enable `girt`'s `http` feature for smart HTTP/HTTPS protocol v0. The adapter performs a service
advertisement GET and, when needed, one upload-pack or receive-pack POST. It does not model HTTP as
a duplex stream. Each upload-pack request contains the complete wants, bounded have batch and
`done`; known-only fetches and empty pushes stop after discovery.

## Runtime and Processing

Create a caller-owned Tokio runtime with I/O and time enabled. The library creates no runtime and no
CPU worker pool. Existing stream and local-process APIs remain synchronous. Core-only builds have no
Tokio, HTTP or TLS dependency.

`fetch::receive_http` takes `Option<Arc<KnownHistory>>` and returns an owned `DownloadedFetch`. Pass
`None` for a full transfer without preparing or allocating history. With `Some`, the result
privately retains the exact history used during negotiation, without copying its objects. Call its
synchronous `validate` method to decode/index the pack, check identities and prove selected-tip
connectivity, then explicitly install the validated result. Validation consumes the download and
releases its Arc on success or failure; dropping the download releases it too. Other Arc owners may
retain history. Installation still rechecks local dependencies; retained history does not coordinate
with GC.

Move the downloaded result into a caller-managed bounded blocking worker for large operations.
Validation can outlive the initiating scope, remote, control and caller's Arc. Even a bounded 512
MiB decode budget can take substantial time. Bound waiting downloads and aggregate retained bytes as
well as running workers. Installation and local history preparation are also synchronous. The
[runnable example](../examples/http_local.rs) shows full and known-only downloads, bounded worker
admission, explicit completion handling and installation outside the executor. It submits only one
worker request; it does not provide a service queue or reusable pool.

`PreparedPush` validates the graph, proves force policy, applies explicit receiver-history exclusion
and generates the ordinary or delta-compressed pack synchronously. Prepare it before calling
`push::send_http`; sending consumes it and moves both existing byte buffers into the HTTP upload.
There is no full-pack copy during the async call. Reprepare after inspecting remote state when an
attempt has an uncertain outcome.

Use `push::send_http_checked` when the application needs to compare its commands with the actual
receive-pack advertisement before sending. Its synchronous callback sees validated reference tips
after discovery. Returning `false` declines the whole batch before POST, so the caller can choose
another transport. `HttpPushOutcome::Declined` proves no command was sent; `Sent` contains the usual
per-reference report. Advertised absence can also mean a hidden ref, and tips may move after the
callback. The receiver still checks every command's expected old value. A failed POST remains
uncertain and must not trigger automatic fallback.

Advertisement parsing, knowledge-budget checks, request selection/encoding and bounded push status
parsing remain synchronous preflight/completion work. Their costs scale with configured ref, known
object, want and status limits; there is no executor latency guarantee for arbitrarily large limits
or slow callbacks. Pack import and filesystem operations are separate. This explicit processing
boundary avoids a hidden `spawn_blocking` job that would continue consuming resources after its
await was dropped.

`HttpRemote` and `DownloadedFetch` are `Send + Sync + 'static`; network futures are `Send` when the
fetch selection callback is `Send`. Inputs borrowed by a future must live until it finishes.
`DownloadedFetch` retains no borrows. Concurrent operations are allowed, with separate resource
budgets; the caller bounds concurrency and aggregate memory.

## Endpoint, Authentication and TLS

Resolve a `Destination` with `HttpSettings::resolve(config, destination, environment)`, then
construct `HttpRemote::configured(settings)`. For a named remote, use
`HttpSettings::resolve_for_remote(config, name, destination, environment)` so its proxy policy
participates. Resolution performs no file or network I/O.

Supported settings are `http.sslVerify`, `http.sslCAInfo`, `http.proxy`,
`http.proxyAuthMethod=basic` and `http.followRedirects`. URL scopes match scheme, host (including
whole-label `*`), effective port and component-bounded path. More specific hosts, then longer paths,
then later entries win; unreserved percent escapes are normalized. Selected unsupported HTTP
settings fail explicitly, including client certificates, pins, CA directories, alternate TLS
backends and proxy TLS policy.

Certificate-chain and hostname verification are enabled by default. With no CA bytes, reqwest's
rustls platform verifier uses native trust. Native trust can depend on OS configuration and
certificate environment variables; girt does not select Git's libcurl backend. Roots, revocation and
distrust behavior can differ from the installed Git. Default-trust Git parity is not claimed. An
explicitly supplied CA bundle replaces platform roots for that connection. Bundles are limited to 1
MiB, may contain multiple certificates, and must contain at least one valid certificate.
`HttpRemote::new` retains its separate contract of adding supplied roots to platform trust.

The application selects `GIT_SSL_CAINFO` first; otherwise
`HttpSettings::configured_ca_info(config, destination)` returns the selected configured path bytes.
The application expands Git path syntax, resolves relative paths against its working directory, and
loads bounded PEM bytes into `HttpEnvironment::ssl_ca_info`. Girt never opens these paths.
Configured paths without supplied bytes fail before network I/O. `ssl_no_verify` overrides
configured verification in both directions. Disabling checks still requires `allow_insecure_tls`
approval.

Proxy precedence is `remote.<name>.proxy`, URL-scoped/global `http.proxy`, then the proxy selected
from environment by the caller. An empty configured proxy disables proxying. HTTP proxies support
HTTPS CONNECT; HTTPS proxies, SOCKS and proxy URL userinfo are rejected. The caller supplies
approved Basic credentials for the effective proxy and optional `no_proxy` exclusions (reqwest
host/domain, IP/CIDR and `*` syntax). Empty exclusions mean no bypass. Girt disables automatic proxy
discovery.

For origin authentication, fill a `remote::CredentialSession` using application-approved helper
programs or prompting, then call `HttpRemote::with_credentials`. The session must match the exact
scheme, authority and repository path. Discovery first tries without the credential; a 401 response
advertising Basic retries that GET once at the same origin. Other challenges receive no credential.
An authenticated 200 approves the session; a 401 rejects it. Subsequent RPCs send the credential to
the bound origin. Helper notification is synchronous, including process startup and callbacks; place
configured network operations on an application worker when executor responsiveness matters. The
operation's deadline and cancellation bounds apply, but synchronous callback and process-start
phases cannot be interrupted. Passwords are never displayed or traced. Plain HTTP transmits
credentials without encryption; use HTTPS across untrusted networks. A push POST is never retried
after an authentication challenge or transport failure because remote application may already have
occurred. Inspect the remote before another push attempt.

Configured connections accept one same-origin initial discovery redirect, retaining its repository
base for the RPC. Cross-origin redirects and redirects with a credential session are refused before
forwarding secrets. `http.followRedirects=false` disables this path; `true` and other values are
refused because their wider semantics are not implemented. Redirect locations and response bodies
are omitted from errors. The explicit `HttpRemote::new` constructor retains its earlier behavior:
caller-supplied `Authorization`, `User-Agent` and `X-*` headers, optional PEM roots, and no proxies
or redirects. Both constructors reject URL userinfo, query and fragment, use HTTP/1.1, refuse
protocol/routing/framing header overrides, and accept only status 200 and exact Git media types.
Cookies and content compression remain disabled. Girt emits no HTTP logs; Git progress and rejection
text remain server-controlled bytes for callers to handle.

## Limits and Interruption

Discovery requires the exact service prelude and flush followed by a valid v0 advertisement. Media
types must match the requested smart Git service without parameters or duplicates. HTTP content
encoding is rejected, avoiding a second decompression budget. Dumb HTTP and protocol v1/v2 are
explicitly unsupported; existing SHA-1 and non-thin-pack constraints apply.

Fetch advertisement and aggregate wire budgets include the smart service prelude. A fetch request
uses at most 96 bytes per want plus 2048 bytes of have/framing overhead. The bounded response is
retained until validation finishes, adding at most `max_wire_bytes` to ordinary fetch memory
budgets. Push owns the already bounded command/pack buffers; status retention uses
`max_status_bytes`. Request/response memory is proportional to configured bounds, not a fixed heap
cap. Response headers are capped at 64 fields and 16 KiB of names/values after parsing. Hyper's
independent parser buffer is currently bounded at approximately 400 KiB before that stricter byte
check. TLS/socket buffers, allocator overhead and server memory are separate.

`TransportControl` polls cancellation at 20 ms intervals during DNS, connect, TLS, upload and
response waits, subject to runtime/OS scheduling. Its absolute deadline spans discovery and RPC and
is never extended by traffic. No deadline means a stalled peer can wait indefinitely until
cancelled. OS DNS resolution can finish in Tokio's resolver pool after cancellation; the adapter
does not wait for it. Already-buffered writes and remote processing cannot be recalled.

Fetch validation uses a separate cancellation flag, checked between packets, objects and graph
steps. It does not inherit the completed network deadline or interrupt a single inflate/hash/parse.
Dropping a download discards it without publication. Installation remains explicit and rechecks
local dependencies.

A push failure before the POST is `NotSent`. Once an RPC is attempted, HTTP errors and interruption
are conservatively `Uncertain`, even when the status suggests rejection. Valid status packets
received before truncation/cancellation are retained, including a complete report if HTTP framing
subsequently fails. Unknown results require remote inspection before retry. Complete Git rejection
or mixed-status reports return `Ok`; inspect each ref. Dropping a polled push future can abandon an
in-flight mutation without a report. Prefer setting cancellation and awaiting the classified result.

## Disposable Example and Fixtures

On macOS/Linux with Git and Python 3 on PATH:

```sh
cargo run --features http --example http_local
```

This creates two private repositories, starts a loopback Python bridge to actual `git http-backend`,
pushes a tag and fetches its object, then removes the fixtures. It never contacts a real remote. The
bridge in `tests/fixtures/http/server.py` is an original test fixture, not a production server. It
can also serve an explicitly disposable bare repository when invoked directly; it prints its
loopback port. Enable that repository's `http.receivepack` setting for pushes.

Run the HTTP suite with Git, Python 3 and OpenSSL installed:

```sh
cargo test --features http --test http
cargo bench --features http --bench http
```

TLS tests generate private one-day CAs and localhost certificates in temporary directories. They
exercise configured HTTPS fetch/push, multi-certificate bundles, untrusted chains, hostname
mismatch, malformed/empty bundles, authenticated CONNECT and proxy bypass without changing platform
trust. An original fixed-loopback CONNECT relay supports the proxy tests. Git executable
observations compare URL-scoped selection and an explicit-CA HTTPS discovery through a per-remote
proxy. Existing fixtures additionally inject HTTP errors, malformed media types, redirects,
truncated and stalled bodies. These cases establish the stated policy on the tested host, not
equivalence across TLS backends or platforms.

## Consumer Eligibility

An HTTPS consumer can use the configured API for anonymous or explicitly approved Basic credentials
with platform trust or a caller-loaded exclusive CA bundle, and optionally an HTTP CONNECT proxy.
Pass the complete effective config snapshot and the selected remote name; do not strip trust
settings to make resolution succeed. Capture proxy and Git TLS environment inputs explicitly. In
particular, keep `SSL_CERT_FILE`, `SSL_CERT_DIR`, `CURL_SSL_BACKEND`,
CA-directory/client-certificate environment, unsupported authentication and other transport
overrides outside the admitted contract until their semantics are mapped and tested. An unsupported
configuration is a preflight decision for the caller; a TLS failure must never trigger disabled
verification or an automatic retry of a push.

For jj integration, first admit the supported policy before network I/O, construct one configured
remote for discovery and RPC, retain existing credential/progress/protocol guards, and test fetch
and push against the same original TLS fixtures. Keep Git fallback for unmapped settings and
required Git-backend trust behavior. Native default verification alone does not justify removing Git
fallback.

Configuration rules are based on [Git's HTTP configuration documentation][git-http]. Explicit trust
replacement uses reqwest's [certificate-root policy][reqwest-trust]. Tests and fixtures are
original; no upstream Git, gix or libgit2 implementation or test source was used.

[git-http]: https://git-scm.com/docs/git-config#Documentation/git-config.txt-httplturlgt
[reqwest-trust]: https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.tls_certs_only

## Dependencies and Scope

Direct optional dependencies are reqwest 0.13.5 (MIT OR Apache-2.0), Tokio 1.4 (MIT), and
futures-util 0.3 (MIT OR Apache-2.0). Reqwest 0.13.5 is needed for its configurable HTTP/1
header-count limit; Tokio 1.4 provides biased selection for interruption polling. The selected TLS
stack uses rustls and AWS-LC. Resolved transitive metadata includes MIT, Apache-2.0, ISC,
BSD-3-Clause, Unicode-3.0 and CDLA-Permissive-2.0 licenses, plus permissive alternatives. AWS-LC
includes combined permissive component notices; distributors must retain applicable dependency
notices. The dependency graph contains no mandatory copyleft license; licenses offering a permissive
alternative are evaluated under that alternative.

SSH is provided by a separate optional [adapter](ssh.md). Credential discovery, remote/refspec
policy, protocol v2, shallow/partial repositories, automatic tags/pruning and thin packs remain
outside this adapter. See [Git compatibility evidence](compatibility.md#smart-http-and-https) and
[loopback benchmarks](benchmarks.md#smart-http-loopback-baseline).
