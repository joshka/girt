---
title: Fetch, push and clone
description: Transfer objects and references over local paths, HTTP(S) and SSH.
---

girt moves objects and references between repositories at two levels. The `workflow` module does
what `git fetch` and `git push` do for a named remote: it reads the configuration, picks the
transport, asks for credentials and updates references. The core `fetch`, `push` and `clone` modules
expose each step instead, so an application chooses its own endpoint, limits and worker threads, and
decides how references are published.

## Choose a level

Use [`workflow`](https://docs.rs/girt/latest/girt/workflow/index.html) when:

- the remote is configured in the repository (`remote.origin.url`), and Git's rules for `insteadOf`,
  `pushurl`, `protocol.allow` and credential helpers should apply;
- the remote, and the repository's own configuration, are trusted the way you trust your own (see
  [Limits and trust](/girt/guides/limits-and-trust/));
- blocking a thread for the whole transfer is acceptable, without cancellation or a deadline.

Use the core APIs when the endpoint comes from somewhere other than configuration, when the peer is
untrusted and the transfer must be bounded, when you need cancellation or a deadline, when downloads
should be validated on your own worker pool, or when you are creating a repository with a clone.

jj's girt backend uses `workflow` for `jj git fetch` and `jj git push`, and the core `remote` module
to list and edit remotes.

## What the caller must exclude

Other processes may update references while a transfer runs: girt publishes a reference only if it
still has the value seen when the transfer started. Reference locks do not protect everything,
though. From the start of a fetch until it returns, keep these from running in the repository (and,
for a push to a local path, in the destination):

- garbage collection or pruning, which could delete objects the transfer depends on;
- checkout, or changes to `HEAD` and the branch a worktree has checked out;
- adding or removing linked worktrees.

## Fetch from a configured remote

Add dependencies with `cargo add girt tempfile`. Each program below runs against disposable local
repositories, without network access. This first program creates a source commit, adds its
repository as a remote, then fetches it. For a network remote, pass its URL as bytes instead of the
local path.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::refs::{Expected, RefName, Target};
    use girt::remote::RemoteConfig;
    use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
    use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};

    let directory = tempfile::tempdir()?;
    let source = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("source.git"),
        InitKind::Bare,
    )?;
    let path = directory.path().join("destination.git");
    Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare)?;
    let loose = source.loose_objects();
    let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    let author = Signature {
        name: b"Example".to_vec(),
        email: b"example@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = loose.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Initial commit\n".to_vec(),
    })?)?;
    source.references()?.update_without_reflog(
        &RefName::new("refs/heads/main")?,
        Target::Direct(commit),
        Expected::Absent,
    )?;
    let url = source
        .git_dir()
        .to_str()
        .ok_or("non-UTF-8 source path")?
        .as_bytes();

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
    assert_eq!(outcome.updated.len(), 1);
    Ok(())
}
```

A fetch goes through these steps:

1. **Resolve the remote.** The first `url` is rewritten by `url.<base>.insteadOf` and checked
   against `protocol.allow` and `GIT_ALLOW_PROTOCOL`.
2. **Choose a transport.** Local paths and `file://` URLs use girt's native local transfer, which
   reads the other repository's storage directly and never starts Git. `http(s)://` needs the `http`
   feature. `ssh://` and `user@host:path` need the `ssh` feature on macOS or Linux.
3. **Negotiate.** Local reference tips are offered as history the repository already has, so the
   server can leave those objects out.
4. **Validate.** Before anything is written, every received object's framing, delta chain and
   identity are checked, and every fetched tip's history is walked to check that it is complete and
   well-formed.
5. **Install.** The pack and its index are written to the repository's object storage.
6. **Publish.** Every planned change is checked against its refspec's rules, such as
   fast-forwarding; if any check fails, no reference changes. Stale references are then pruned in
   one transaction, and the other references updated in a second. Each change requires the reference
   to still have the value it had when the fetch began, so a reference another process moved
   meanwhile is never overwritten. Because the two transactions are separate, such a conflict can be
   reported after pruning has happened.

Some behavior differs from `git fetch`. Destinations must be remote-tracking references, tags, or a
namespace you allow with `FetchOptions::tag_namespace`, so a fetch can never move a checked-out
branch. No `FETCH_HEAD` is written. Tags are not followed automatically: fetch the tags you want
with a refspec such as `refs/tags/*:refs/tags/*`. A missing exact source is reported in
`FetchOutcome::missing_sources` instead of failing the fetch.

An open `Objects` reader keeps the pack list it started with. Open a new one after fetching to read
the new objects; jj reopens its object store after every fetch for this reason.

### When a fetch fails

`workflow::fetch` returns `TransferError`. Most variants mean nothing changed locally.
`TransferError::FetchFinish` means the pack was received and validated, but installing it or
publishing the results failed partway. Its `FetchFinishError` holds a report and the failure:

- `report.installed` is set only if the pack was installed. A failed installation can leave an
  unindexed pack file behind.
- `report.shallow_published` says whether `.git/shallow` changed.
- A failed reference transaction carries the references it did publish.

Nothing is rolled back. Fetching again is safe, and brings the references up to date.

## Push

A push sends a list of `PushCommand`s. Each command names a remote reference, the value you believe
it has now (`None` for "must not exist"), and the new value. The null ID deletes the reference. The
remote applies a command only if the reference still has the expected value. This is Git's
`--force-with-lease`, and it is always on. A command whose reference already has the new value
succeeds without comparing the expected value, since nothing needs to change. `ForcePolicy`
separately decides whether a branch may move to a commit that does not descend from its old value.

This program creates a commit in `repo` and pushes it to an empty local remote:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::push::{ForcePolicy, PushCommand};
    use girt::refs::{RefName, Target};
    use girt::workflow::{self, Environment, NoCallbacks, PushOptions};
    use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};

    let directory = tempfile::tempdir()?;
    let destination = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("destination.git"),
        InitKind::Bare,
    )?;
    let path = directory.path().join("source.git");
    Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare)?;
    let repo = Repository::open(&path)?;
    let loose = repo.loose_objects();
    let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    let author = Signature {
        name: b"Example".to_vec(),
        email: b"example@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = loose.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Initial commit\n".to_vec(),
    })?)?;
    let mut edit = repo.edit_config(1 << 20)?;
    girt::remote::RemoteConfig::new(edit.document_mut()).add(
        b"origin",
        destination
            .git_dir()
            .to_str()
            .ok_or("non-UTF-8 destination path")?
            .as_bytes(),
        &[],
    )?;
    edit.commit()?;
    let repo = Repository::open(&path)?;

    let topic = RefName::new("refs/heads/topic")?;
    let command =
        PushCommand::new(topic.clone(), None, commit).with_force(ForcePolicy::FastForwardOnly);
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
    assert!(report.all_succeeded());
    assert_eq!(
        destination.references()?.read(&topic)?,
        Some(Target::Direct(commit))
    );
    Ok(())
}
```

Commands succeed or fail independently, so check the report even when `push` returns `Ok`. A failed
push arrives as `TransferError::Push`, holding a `PushError` that says how far it got:

- `PushError::NotSent` means the push attempted no reference updates on the remote. A push to a
  local path may still have stored objects or run receive hooks there before failing.
- `PushError::Uncertain` means some updates may have been applied. Read the remote's references, for
  example with a fetch, before trying again.

The pack leaves out history the remote advertises, when that history exists locally, so creating a
branch usually sends only the commits the remote lacks. After a successful update, the matching
remote-tracking reference is updated locally, as `git push` does. That update is best effort; fetch
again if the local view must be exact.

## Clone

A clone is a fetch into a repository that does not exist yet. The destination must not exist, and
its parent directory must. With the core API:

1. `CloneRequest::prepare_tracking` chooses the destination path, bare or ordinary layout, the
   stored origin URL, which branch becomes the local branch, and how reflogs are written. Nothing is
   created yet.
2. Receive from an explicit endpoint. `receive_local` transfers and validates from a path. The HTTP
   and SSH adapters return an owned download for you to validate on a worker thread of your choice.
3. `CloneReady::finish` creates the repository, installs the pack, writes the origin configuration
   and publishes `refs/remotes/origin/*`, tags, the selected local branch and `HEAD`.

Every remote branch lands in `refs/remotes/origin/*`, with at most one local branch, even for a bare
clone. This differs from `git clone --bare`, which copies every branch into `refs/heads/*`. The
clone writes no working files and no index, which is Git's `--no-checkout` state. Populate the
working tree afterwards with `Repository::checkout_tree`. If `finish` fails, `CloneError` reports
which phases completed. girt never deletes a partially created destination for you.

## The core fetch API

`workflow` is built from the core APIs. Used directly, each phase is a separate call with its own
limits and controls:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use girt::refs::{Expected, RefName, Target};
    use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use girt::fetch::{
        FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory, LocalFetchObserver,
    };
    use girt::refs::Reflog;
    use girt::remote::Remote;
    use girt::transport::TransportControl;

    let directory = tempfile::tempdir()?;
    let source = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("source.git"),
        InitKind::Bare,
    )?;
    let path = directory.path().join("destination.git");
    Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare)?;
    let loose = source.loose_objects();
    let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    let author = Signature {
        name: b"Example".to_vec(),
        email: b"example@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = loose.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Initial commit\n".to_vec(),
    })?)?;
    source.references()?.update_without_reflog(
        &RefName::new("refs/heads/main")?,
        Target::Direct(commit),
        Expected::Absent,
    )?;
    let mut edit = Repository::open(&path)?.edit_config(1 << 20)?;
    girt::remote::RemoteConfig::new(edit.document_mut()).add(
        b"origin",
        b"local-source",
        &[b"refs/heads/*:refs/remotes/origin/*"],
    )?;
    edit.commit()?;
    let repo = Repository::open(&path)?;
    let source_path = source.git_dir();

    let origin = Remote::find(repo.config(), b"origin")?.ok_or("no origin")?;
    // Capture the destination's references. No forced destinations are authorized here, so `+`
    // refspecs still refuse non-fast-forward updates.
    let request = FetchRequest::prepare(
        repo.clone(),
        origin.fetch_refspecs().clone(),
        BTreeSet::new(),
        Reflog::Preserve,
    )?;

    // Transfer and validate. The deadline covers this call only.
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel)
        .with_deadline(Some(Instant::now() + Duration::from_secs(120)));
    let ready = request.receive_local(
        source_path,
        &KnownHistory::default(),
        FetchLimits::default(),
        control,
        LocalFetchObserver::default(),
    )?;

    // Install and publish, checking `cancel` between steps.
    match ready.finish(FetchUpdateLimits::default(), &cancel) {
        Ok(report) => println!("published {} references", report.references.len()),
        Err(error) => {
            eprintln!(
                "partial effects: {:?}; failure: {}",
                error.report, error.source
            );
            return Err(error.into());
        }
    }
    assert_eq!(
        repo.references()?
            .read(&RefName::new("refs/remotes/origin/main")?)?,
        Some(Target::Direct(commit)),
    );
    Ok(())
}
```

Here `source_path` is the path of a local source repository. With the `http` or `ssh` feature,
`FetchRequest::receive_http` and `receive_ssh` replace `receive_local`. They are `async`, run on
your Tokio runtime, and return a `FetchDownload`. Call its synchronous `validate`, preferably on a
blocking worker, to get the `FetchReady`.

A `TransportControl` deadline covers the transfer call it is passed to. Validation of a network
download and `finish` take only a cancellation flag. To bound the whole operation, set the flag from
a timer and wait for the worker to return.

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
