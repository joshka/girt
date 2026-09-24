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

Construct `transport::http::HttpRemote` with a repository URL, explicit headers and optional PEM
trust roots. URLs must omit userinfo, query and fragment. Up to 8192 URL bytes and 16 KiB of
supplied header names/values are accepted. Allowed headers are `Authorization`, `User-Agent` and
`X-*`; protocol, routing, proxy and framing headers cannot be overridden. All supplied values are
marked sensitive. A credential applies to both service paths at the same configured repository.

Supply a complete authorization header up front. There is no challenge retry, credential-helper,
keychain, cookie or remote-config discovery. Plain HTTP transmits credentials without encryption;
use HTTPS across untrusted networks. HTTPS uses rustls with platform certificate trust and any
explicit additional trust roots. Certificate and hostname verification cannot be disabled through
this API. HTTP/1.1 is used even when a server supports HTTP/2.

All redirects are rejected, including same-origin redirects. Their locations and response bodies are
not included in errors. Proxy discovery and use are disabled, including environment proxies. Only
HTTP status 200 is accepted; 401/403, 3xx, 4xx and 5xx return their status code without the body.
The adapter never retries an exchange. It emits no logs and redacts endpoint/header data from its
Debug representation and HTTP errors. Git progress and rejection reasons remain server-controlled
bytes; callers decide whether to display them.

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

TLS tests generate a private one-day CA and localhost certificate in temporary directories. They
exercise trusted HTTPS fetch/push, untrusted chains and hostname mismatch without changing platform
trust. Fixtures additionally inject HTTP errors, malformed headers/media types, redirects, truncated
and stalled bodies. The HTTP tests are Unix-gated; runtime evidence currently covers macOS arm64.
Configured Linux CI is future validation, not evidence that this revision has run there.

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
policy, protocol v2, shallow/partial repositories, automatic tags/pruning, thin packs and push
deletion remain outside this adapter. See
[Git compatibility evidence](compatibility.md#smart-http-and-https) and
[loopback benchmarks](benchmarks.md#smart-http-loopback-baseline).
