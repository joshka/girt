---
title: Fetch, push and clone
description: Transfer objects and references over local paths, HTTP(S) and SSH.
---

girt moves objects and references between repositories at two levels. The `workflow` module does
what `git fetch` and `git push` do for a named remote: it reads the configuration, picks the
transport, asks for credentials and updates references. The core `fetch`, `push` and `clone` modules
expose each step instead, so an application can choose its own endpoint, limits, worker threads and
publication policy. This page explains both levels and when to use each.

## Choose a level

Use [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) when:

- the remote is configured in the repository (`remote.origin.url`), and Git's rules for `insteadOf`,
  `pushurl`, `protocol.allow` and credential helpers should apply;
- the remote is trusted the way you trust your own remotes (see
  [Limits and trust](/girt/guides/limits-and-trust/));
- blocking a thread for the whole transfer is acceptable, without cancellation or a deadline.

Use the core APIs when the endpoint comes from somewhere other than configuration, when the peer is
untrusted and the transfer must be bounded, when you need cancellation or a deadline, when downloads
should be validated on your own worker pool, or when you are creating a repository with a clone.

jj's girt backend uses `workflow` for `jj git fetch` and `jj git push`, and the core `remote` module
to list and edit remotes.

## Fetch from a configured remote

Add a remote, then fetch. In this example `url` is a local repository path; an `https://` or
`ssh://` URL takes the same code path once the `http` or `ssh` feature is enabled.

```rust
use girt::Repository;
use girt::remote::RemoteConfig;
use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};

// Record the remote in the repository's own config file, as `git remote add` would.
let mut edit = Repository::open(&path)?.edit_config(1 << 20)?;
RemoteConfig::new(edit.document_mut()).add(
    b"origin",
    url,
    &[b"+refs/heads/*:refs/remotes/origin/*"],
)?;
edit.commit()?;

// A `Repository` holds a configuration snapshot, so reopen it to see the new remote.
let repo = Repository::open(&path)?;
let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]).prune(true);
let outcome = workflow::fetch(
    &repo,
    "origin",
    &options,
    &Environment::from_process(),
    &mut NoCallbacks,
)?;
for change in &outcome.updated {
    println!("{:?}: {:?} -> {:?}", change.name, change.old, change.new);
}
```

A fetch goes through these steps:

1. **Resolve the remote.** The first `url` is rewritten by `url.<base>.insteadOf` and checked
   against `protocol.allow` and `GIT_ALLOW_PROTOCOL`.
2. **Choose a transport.** Local paths and `file://` URLs use girt's native local transfer, which
   reads the other repository's storage directly and never starts Git. `http(s)://` needs the `http`
   feature. `ssh://` and `user@host:path` need the `ssh` feature on macOS or Linux.
3. **Negotiate.** Local reference tips are offered as history the repository already has, so the
   server can leave those objects out. HTTP and SSH ask for protocol v2 unless `protocol.version`
   says otherwise, and accept a v0 or v1 answer.
4. **Validate.** The received pack is checked in full before anything is written: pack framing,
   every object's identity, delta chains, and that every fetched tip's history is complete.
5. **Install, then publish.** The pack is installed. The reference updates are then checked as one
   batch: each destination must still have the value it had when the fetch began, and each move must
   be allowed by its refspec. If any check fails, no reference changes. That way a reference another
   process moved during the transfer is never overwritten.

Some behavior differs from `git fetch`. Destinations must be remote-tracking references, tags, or a
namespace you allow with `FetchOptions::tag_namespace`, so a fetch can never move a checked-out
branch. No `FETCH_HEAD` is written. Tags are not followed automatically: fetch the tags you want
with a refspec such as `refs/tags/*:refs/tags/*`. A missing exact source is reported in
`FetchOutcome::missing_sources` instead of failing the fetch.

An open `Objects` reader keeps the pack list it started with. Open a new one after fetching to read
the new objects; jj reopens its object store after every fetch for this reason.

## Push

A push sends a list of `PushCommand`s. Each command names a remote reference, the value you believe
it has now (`None` for "must not exist"), and the new value. The null ID deletes the reference. The
remote applies a command only if the reference still has the expected value. This is Git's
`--force-with-lease`, and it is always on. `ForcePolicy` separately decides whether a branch may
move to a commit that does not descend from its old value.

```rust
use girt::push::{ForcePolicy, PushCommand};
use girt::refs::RefName;
use girt::workflow::{self, Environment, NoCallbacks, PushOptions};

let topic = RefName::new("refs/heads/topic")?;
let command = PushCommand::new(topic, None, commit).with_force(ForcePolicy::FastForwardOnly);
let report = workflow::push(
    &repo,
    "origin",
    vec![command],
    &PushOptions::default(),
    &Environment::from_process(),
    &mut NoCallbacks,
)?;
if !report.all_succeeded() {
    for status in &report.refs {
        println!("{:?}: {:?}", status.command.name, status.status);
    }
}
```

Commands succeed or fail independently, so check the report even when `push` returns `Ok`. When a
push fails, the error says whether anything may have reached the remote. `PushError::NotSent`
guarantees nothing did. `PushError::Uncertain` means some updates may have been applied: read the
remote's references, for example with a fetch, before trying again.

The pack leaves out history the remote already advertises, so creating a branch sends only the
commits the remote lacks. After a successful update, the matching remote-tracking reference is
updated locally, as `git push` does.

## Clone

A clone is a fetch into a repository that does not exist yet. With the core API:

1. `CloneRequest::prepare_tracking` chooses the destination path, bare or ordinary layout, the
   stored origin URL, which branch to check out, and the reflog policy. Nothing is created yet.
2. Receive from an explicit endpoint. `receive_local` transfers and validates from a path. The HTTP
   and SSH adapters return an owned download for you to validate on a worker thread of your choice.
3. `CloneReady::finish` creates the repository, installs the pack, writes the origin configuration
   and publishes `refs/remotes/origin/*`, tags, the selected local branch and `HEAD`.

The clone copies no working files and creates no index, which is Git's `--no-checkout` state.
Populate the working tree afterwards with `Repository::checkout_tree`. If `finish` fails,
`CloneError` reports which phases completed. girt never deletes a partially created destination for
you.

## The core fetch API

`workflow` is built from the core APIs, which keep every step visible:

- `FetchRequest::prepare` records the destination's current references, the refspecs, which forced
  destinations you authorize, and the reflog policy.
- `receive_local`, or `receive_http` and `receive_ssh` with the features enabled, transfers and
  validates. The network variants are `async` and run on your Tokio runtime. They return a download
  that you validate synchronously, preferably on a blocking worker.
- `FetchReady::finish` installs the objects and publishes the references, returning a `FetchReport`.
  If it fails, `FetchFinishError` carries the report of what was already done.

Every step takes explicit `FetchLimits` and a `TransportControl`, which carries a cancellation flag
and an optional deadline. Each step checks them between units of work.

## Progress

`workflow` reports progress through `TransferCallbacks`. `remote_message` receives the server's
`remote:` lines. They need not be UTF-8, may arrive in pieces, and may end in `\r` to be redrawn.
`transport_message` receives SSH's standard error. `validation_progress` counts objects validated
during a local transfer. The core APIs take the same information through `LocalFetchObserver`,
progress closures and `validate_with_progress`.

## Reference

- API docs on docs.rs: [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html),
  [`remote`](https://docs.rs/girt/latest/girt/remote/index.html),
  [`fetch`](https://docs.rs/girt/latest/girt/fetch/index.html),
  [`clone`](https://docs.rs/girt/latest/girt/clone/index.html),
  [`push`](https://docs.rs/girt/latest/girt/push/index.html),
  [`transport`](https://docs.rs/girt/latest/girt/transport/index.html).
- Runnable examples:
  [`fetch_remote`](https://github.com/joshka/girt/blob/main/examples/fetch_remote.rs),
  [`clone_repository`](https://github.com/joshka/girt/blob/main/examples/clone_repository.rs),
  [`push_local`](https://github.com/joshka/girt/blob/main/examples/push_local.rs),
  [`http_local`](https://github.com/joshka/girt/blob/main/examples/http_local.rs),
  [`ssh_local`](https://github.com/joshka/girt/blob/main/examples/ssh_local.rs).
- Related guides: [Credentials and transports](/girt/guides/credentials-and-transports/) and
  [Limits and trust](/girt/guides/limits-and-trust/).
