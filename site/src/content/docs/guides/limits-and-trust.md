---
title: Limits and trust
description: Why girt bounds network input, and which limits to choose for trusted and untrusted remotes.
---

Every transfer API in girt takes explicit resource limits: `FetchLimits`, `FetchUpdateLimits`,
`PushLimits` and the `PackLimits` used to open object stores. Each limits type has two constructors,
`default()` and `trusted()`. Which one to use depends on who chose the other end of the transfer.

## Validation is not the same as a limit

girt validates everything it receives, whatever the limits: pack framing and checksums, every
object's identity and syntax, delta chains, and the connectivity of every fetched tip. Malformed or
corrupt input is always rejected, and nothing is installed until validation finishes.

Limits answer a different question: how much well-formed work may the other side ask for? A pack can
be valid and still be 50 GB, contain ten million objects, or chain deltas thousands deep. A Git
client accepts all of that from the remotes its user configured. A service that fetches URLs its
users submit must not, or one user could exhaust its disk, memory or CPU time.

## Trusted: as Git trusts your own remotes

`FetchLimits::trusted()`, `PushLimits::trusted()`, `FetchUpdateLimits::trusted()` and
`PackLimits::trusted()` set every bound so high that only available memory and disk limit the work.
That matches `git fetch`, which has no such limits. The `workflow` module always uses trusted
limits, because it acts for a user on remotes that user configured.

Use trusted limits for a remote the user chose, and when opening a large local repository you trust.
The defaults are sized for untrusted input and refuse very large stores. For example, the default
`PackLimits` admits 64 MiB of pack index, while a Linux kernel clone has about 300 MB.

## Default: bounded for untrusted input

`FetchLimits::default()` bounds one fetch to:

| Bound                      | Default                   |
| -------------------------- | ------------------------- |
| Bytes received on the wire | 512 MiB                   |
| Reference advertisement    | 4 MiB, 100,000 references |
| Received pack size         | 256 MiB                   |
| Objects in the pack        | 1,000,000                 |
| Size of one decoded object | 64 MiB                    |
| Delta chain depth          | 64                        |
| Total decoding work        | 512 MiB                   |
| Reachability edges checked | 4,000,000                 |
| Worker threads             | 8                         |

Use these, or tighter values, when the remote is chosen by someone other than the person running the
process: a CI service, a mirror, a code-search indexer. Combine them with a deadline, since limits
bound work, not time:

```rust
use std::time::{Duration, Instant};

let limits = FetchLimits::default()
    .with_max_pack_bytes(64 * 1024 * 1024)
    .with_max_objects(200_000);
let control = TransportControl::new(&cancel)
    .with_deadline(Some(Instant::now() + Duration::from_secs(120)));
```

A transfer that reaches a limit fails before anything is installed, and the error names the bound,
such as `fetch limit exceeded: …`. Nothing needs cleaning up.

The limits bound input and work, not total process memory. Allocator overhead, TLS buffers and
memory on the server are outside them. Each transfer has its own budget, so an application running
several transfers at once must also bound how many run concurrently.

## Cancellation and deadlines

The core APIs take a `TransportControl` that carries a cancellation flag (`AtomicBool`) and an
optional deadline. They are checked between units of work. A single hash, inflate or filesystem call
is not interrupted. Network adapters poll the flag while waiting for the peer. Without a deadline, a
stalled peer can hold a transfer until it is cancelled.

For pushes, how far the push got decides what an interruption means. A push interrupted before any
command was sent returns `PushError::NotSent`, and the remote is unchanged. One interrupted after
sending began returns `PushError::Uncertain`: inspect the remote before retrying.

`workflow` takes no `TransportControl`. Its transfers can be neither cancelled nor given a deadline,
which is another reason to use the core APIs for untrusted remotes.

## Reference

- [`FetchLimits`](https://docs.rs/girt/latest/girt/fetch/struct.FetchLimits.html),
  [`PushLimits`](https://docs.rs/girt/latest/girt/push/struct.PushLimits.html),
  [`PackLimits`](https://docs.rs/girt/latest/girt/struct.PackLimits.html) and
  [`TransportControl`](https://docs.rs/girt/latest/girt/transport/struct.TransportControl.html)
  on docs.rs.
- [Fetch, push and clone](/girt/guides/fetch-push-clone/) and the
  [safety model](/girt/compatibility/safety-model/).
