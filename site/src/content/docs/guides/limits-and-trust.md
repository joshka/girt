---
title: Limits and trust
description: Why girt bounds network input, and which limits to choose for trusted and untrusted remotes.
---

Every transfer API in girt takes explicit resource limits: `FetchLimits`, `FetchUpdateLimits`,
`PushLimits` and the `PackLimits` used to open object stores. Each limits type has two constructors,
`default()` and `trusted()`. Which one to use depends on who chose the other end of the transfer.

## Validation is not the same as a limit

girt validates what it receives, whatever the limits. It checks every received object's pack
framing, checksum, delta chain and identity, then walks the history of every fetched tip down to any
shallow boundary: each object reached must be present and well-formed. Malformed objects outside
that history, such as extras a server included, are not parsed further and do not fail the fetch.
Nothing is installed until validation finishes.

Limits bound how much well-formed work the other side may ask for. A pack can be valid and still be
50 GB, contain ten million objects, or chain deltas thousands deep. A Git client accepts all of that
from the remotes its user configured. A service that fetches URLs its users submit must not, or one
user could exhaust its disk, memory or CPU time.

## Trusted: as Git trusts your own remotes

`FetchLimits::trusted()`, `PushLimits::trusted()`, `FetchUpdateLimits::trusted()` and
`PackLimits::trusted()` raise the size and count limits so far that available memory and disk are
what bound the work, matching `git fetch`. They keep a few limits that shape how girt works rather
than how much it accepts: for example, at most 256 commits are offered during negotiation, a
transfer adds at most 8 worker threads, delta chains may be at most 10,000 deep, and at most 4,096
files are kept open. The docs.rs pages list each value.

The `workflow` module always uses trusted limits, because it acts for a user on remotes that user
configured. Use them yourself for a remote the user chose, and when opening a large local repository
you trust. The defaults are sized for untrusted input and refuse very large stores: the default
`PackLimits` admits 64 MiB of pack index, while a Linux kernel clone has about 300 MB.

## Repo and explicit control

[`Repo`](https://github.com/joshka/girt/blob/main/src/repo/handle.rs) is the convenience interface
for trusted local use. It loads objects lazily with the core default limits; its name does not
select the larger `trusted()` bounds. Its configured transfers compose `workflow` and follow that
module's trust contract. Choose
[`Repository`](https://docs.rs/girt/latest/girt/repository/struct.Repository.html) for explicit
limits and cancellation, especially when processing untrusted input.

## Default: bounded for untrusted input

`FetchLimits::default()` bounds the transfer and validation of one fetch:

| Bound                                   | Default                   |
| --------------------------------------- | ------------------------- |
| Bytes received on the wire              | 512 MiB                   |
| Reference advertisement                 | 4 MiB, 100,000 references |
| Received pack size                      | 256 MiB                   |
| Objects in the pack                     | 1,000,000                 |
| Size of one decoded object              | 64 MiB                    |
| Delta chain depth                       | 64                        |
| Decoding while validating received data | 512 MiB                   |
| Reachability edges checked              | 4,000,000                 |
| Worker threads                          | 8                         |

The decoding allowance covers inflating and reconstructing the received objects. A native local
fetch applies a separate allowance of the same size to selecting objects in the source and to
writing the pack. Reading the destination's own history while negotiating, and checking the results
before publishing, have budgets of their own in `FetchLimits` and `FetchUpdateLimits`.

Use these defaults, or tighter values, when the remote is chosen by someone other than the person
running the process: a CI service, a mirror, a code-search indexer.

```rust
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::FetchLimits;
use girt::transport::TransportControl;

let limits = FetchLimits::default()
    .with_max_pack_bytes(64 * 1024 * 1024)
    .with_max_objects(200_000);
let cancel = AtomicBool::new(false);
let control = TransportControl::new(&cancel)
    .with_deadline(Some(Instant::now() + Duration::from_secs(120)));
```

## What happens at a limit

A transfer that reaches a `FetchLimits` bound while receiving or validating fails before anything is
installed, with an error such as `fetch limit exceeded: …`. The repository is unchanged.

`FetchUpdateLimits` bounds `finish`. Its snapshot limits apply while opening the destination to
check dependencies, before installation. Its verification and history limits apply after the pack is
installed, and possibly after new shallow boundaries are published, while `finish` checks the
installed history and decides which updates fast-forward. Either way no references are updated, and
the error's report says what was installed or published. Nothing is rolled back or cleaned up
automatically.

The limits bound input and work, not total process memory. Allocator overhead, TLS buffers and
memory on the server are outside them. Each transfer has its own budget, so an application running
several transfers at once must also bound how many run concurrently.

## Cancellation and deadlines

Limits bound work, not time. The core transfer calls take a `TransportControl`, which carries a
cancellation flag (`AtomicBool`) and an optional absolute deadline. girt checks them between units
of work, so a single hash, inflate or filesystem call is not interrupted; network adapters also poll
them while waiting for the peer. Without a deadline, a stalled peer can hold a transfer until it is
cancelled.

The deadline covers only the call it is passed to. Validating a network download and `finish` take
just the cancellation flag. To bound a whole fetch, including validation and publication, set the
flag from a timer and wait for the call to return.

For pushes, how far the push got decides what an interruption means. `PushError::NotSent` means the
push attempted no reference updates on the remote; a push to a local path may still have stored
objects or run receive hooks. `PushError::Uncertain` means some updates may have been applied:
inspect the remote before retrying.

`workflow` takes no `TransportControl`. Its transfers can be neither cancelled nor given a deadline,
which is another reason to use the core APIs for untrusted remotes.

## Reference

- [`FetchLimits`](https://docs.rs/girt/latest/girt/fetch/struct.FetchLimits.html),
  [`FetchUpdateLimits`](https://docs.rs/girt/latest/girt/fetch/struct.FetchUpdateLimits.html),
  [`PushLimits`](https://docs.rs/girt/latest/girt/push/struct.PushLimits.html),
  [`PackLimits`](https://docs.rs/girt/latest/girt/objects/struct.PackLimits.html) and
  [`TransportControl`](https://docs.rs/girt/latest/girt/transport/struct.TransportControl.html) on
  docs.rs.
- [Fetch, push and clone](/girt/guides/fetch-push-clone/) and the
  [safety model](/girt/compatibility/safety-model/).
