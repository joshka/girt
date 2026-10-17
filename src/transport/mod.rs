//! Transports, protocol versions, resource limits, and cancellation and deadlines.
//!
//! A transfer reaches another repository through one of three transports. This module holds the
//! network transports' endpoint types, the [`TransportControl`] that cancels a transfer or imposes
//! a deadline, and the [`RequestedProtocol`] for fetches. The transfer operations themselves live
//! in [`crate::fetch`], [`crate::push`] and [`crate::clone`]; [`crate::workflow`] picks a transport
//! from configuration automatically.
//!
//! # Choosing a transport
//!
//! | Remote | Transport | Feature | Endpoint |
//! | --- | --- | --- | --- |
//! | Local path or `file://` URL | Native local | none | a path |
//! | `http://`, `https://` | Smart HTTP | `http` | [`http::HttpRemote`] |
//! | `ssh://`, `user@host:path` | System OpenSSH, macOS and Linux | `ssh` | [`ssh::SshRemote`] |
//!
//! **Native local** transfers read the other repository's object storage directly. They never
//! start `git upload-pack` or `git receive-pack`, need no Git installation and no async runtime,
//! and are synchronous. A local push runs the destination's receive hooks and follows its
//! `receive.*` settings.
//!
//! **Smart HTTP** uses reqwest with rustls. [`http::HttpRemote::configured`] applies Git's `http.*`
//! configuration: TLS verification and CA bundles, proxies and same-origin redirects.
//! [`http::HttpRemote::new`] instead takes explicit headers, for applications that supply their
//! own `Authorization` header.
//!
//! **SSH** runs the system `ssh` executable, so keys, agents, `known_hosts` and `~/.ssh/config`
//! work as they do for the `ssh` command. [`ssh::SshRemote::openssh`] keeps the user's ordinary
//! OpenSSH configuration, which is what an interactive client such as jj wants.
//! [`ssh::SshRemote::configured`] and [`ssh::SshRemote::new`] instead pin an explicit
//! configuration file and force restrictive options (no password prompts, forwarding or
//! connection sharing, and agents or askpass only when the application supplies them), for
//! services that must not depend on a user's settings.
//!
//! `git://` and remote helpers (`<transport>::<address>`) are not supported.
//!
//! # Async network, synchronous validation
//!
//! The HTTP and SSH adapters are `async` and run on a Tokio runtime the caller owns, with I/O and
//! time enabled. girt creates no runtime and spawns no tasks. A network fetch returns an owned
//! download, such as [`crate::fetch::FetchDownload`], rather than validated objects. Validation
//! inflates, hashes and parses every object, which can take seconds of CPU time, so it is a
//! separate synchronous call. Run it on a blocking worker (`tokio::task::spawn_blocking` or your
//! own pool), not on an executor thread. The caller bounds how many downloads wait and how many
//! validations run at once, and joins each worker even after cancelling it.
//!
//! [`crate::workflow`] hides this split by creating a private single-threaded runtime per call;
//! do not call it from inside another Tokio runtime.
//!
//! # Protocol versions
//!
//! Git has three versions of the fetch (upload-pack) protocol. Version 2 sends capabilities first
//! and lets the client run separate `ls-refs` and `fetch` commands; versions 0 and 1 begin with the
//! full reference advertisement. [`RequestedProtocol`] selects which version an HTTP or SSH fetch
//! asks for. A server asked for v2 may answer with v0 or v1, and girt accepts that, so requesting
//! v2 from an older server is safe. A v2 answer to a v0 or v1 request is refused. The core
//! adapters request v0 by default; [`crate::workflow`] follows `protocol.version`, which defaults
//! to v2 as in Git. Push uses the v0 or v1 receive-pack protocol, which has no v2, and native local
//! transfers use no wire protocol.
//!
//! # Limits and trust
//!
//! Every transfer validates everything it receives, whatever its limits: pack framing and
//! checksums, each object's identity and syntax, delta chains, and that the history of every
//! fetched tip is complete. Malformed input is always rejected, and nothing is installed until
//! validation succeeds.
//!
//! Resource limits answer a different question: how much *well-formed* work may the other side
//! ask for? A valid pack can be 50 GB, hold ten million objects or chain deltas thousands deep.
//! `git fetch` accepts all of that from the remotes its user configured. A service that fetches
//! URLs supplied by its users must not, or one user can exhaust its disk, memory or CPU.
//!
//! Each phase of a transfer therefore takes explicit limits, and each limits type has two
//! constructors:
//!
//! | Limits | Bounds | Untrusted input | Trusted input |
//! | --- | --- | --- | --- |
//! | [`FetchLimits`](crate::fetch::FetchLimits) | advertisement, wire bytes, pack size, objects, decoding, connectivity | `default()` | `trusted()` |
//! | [`FetchUpdateLimits`](crate::fetch::FetchUpdateLimits) | checking installed objects and fast-forwards before publishing | `default()` | `trusted()` |
//! | [`PushLimits`](crate::push::PushLimits) | advertisement, commands, selected history, pack writing, status | `default()` | `trusted()` |
//! | [`PackLimits`](crate::PackLimits) | opening an object store: index bytes, packs, files | `default()` | `trusted()` |
//!
//! `default()` suits a peer you don't control: for example, a fetch may receive at most 512 MiB
//! on the wire and a 256 MiB pack of at most one million objects, with each object at most 64 MiB
//! and deltas at most 64 deep. `trusted()` raises the size and count limits until available memory
//! and disk are what bound the work, matching Git; [`crate::workflow`] uses it for the remotes a
//! user configured. It keeps a few limits that shape how girt works rather than how much it
//! accepts, such as the number of commits offered during negotiation, worker threads and open
//! files; each type's `trusted()` documents its values.
//! The defaults also refuse large *trusted* repositories. The default
//! [`PackLimits`](crate::PackLimits) admits 64 MiB of pack index, while a Linux kernel clone has
//! about 300 MB, so use `trusted()` when opening big repositories you control, as in
//! [`FetchLimits::source_packs`].
//!
//! Exceeding a [`FetchLimits`](crate::fetch::FetchLimits) bound while receiving or validating
//! fails the transfer before anything is installed, with an error that names the bound, such as
//! [`FetchError::Limit`](crate::fetch::FetchError::Limit); the repository is unchanged.
//! [`FetchUpdateLimits`](crate::fetch::FetchUpdateLimits) apply later, in
//! [`FetchReady::finish`](crate::fetch::FetchReady::finish), after the pack is installed: exceeding
//! one there publishes no references, but leaves the installed objects, and reports them in
//! [`FetchFinishError`](crate::fetch::FetchFinishError). Limits bound input and work, not
//! wall-clock time or total process memory: pair them with a deadline, and bound how many
//! transfers run at once.
//!
//! [`FetchLimits::source_packs`]: crate::fetch::FetchLimits::source_packs
//!
//! # Cancellation and deadlines
//!
//! [`TransportControl`] carries a cancellation flag and an optional absolute deadline into every
//! core transfer. Set the [`AtomicBool`](std::sync::atomic::AtomicBool) from another thread to stop
//! a transfer; the deadline is never extended by traffic. Both are checked between units of work
//! and, for the network adapters, every 20 ms while waiting for the peer. A single hash, inflate or
//! filesystem call is not interrupted.
//!
//! What an interruption means depends on how far the transfer got. A fetch interrupted before
//! installation leaves the repository unchanged; one interrupted after installation leaves the
//! new objects in place but publishes no references, and reports what was done in
//! [`FetchFinishError`](crate::fetch::FetchFinishError). A push interrupted before any command was
//! sent returns [`PushError::NotSent`](crate::push::PushError::NotSent): it attempted no reference
//! updates, though a push to a local path may already have stored objects or run receive hooks
//! there. One interrupted after sending began returns
//! [`PushError::Uncertain`](crate::push::PushError::Uncertain), because the remote may already have
//! applied some updates. Read the remote's references before retrying it.
//!
//! A deadline covers only the call it is passed to. Validating a network download and
//! [`FetchReady::finish`](crate::fetch::FetchReady::finish) take just the cancellation flag; to
//! bound a whole fetch, set the flag from a timer and wait for the call to return.
//!
//! [`crate::workflow`] takes no control: its transfers can be neither cancelled nor bounded in
//! time.
//!
//! ## Example: a deadline before sending
//!
//! A deadline that has already passed stops a native local push (setup hidden) before any command
//! is sent, so `NotSent` guarantees the destination references are unchanged. An interruption
//! after sending begins is reported as `Uncertain` instead; inspect the destination before
//! retrying.
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//! use std::time::Instant;
//!
//! use girt::PackLimits;
//! use girt::push::{
//!     ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits,
//!     send_local,
//! };
//! use girt::transport::TransportControl;
//! # use girt::refs::{Expected, RefName, Target};
//! # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
//! # let directory = tempfile::tempdir()?;
//! # let source_path = directory.path().join("source.git");
//! # let source = Repository::init(ObjectFormat::Sha1, source_path, InitKind::Bare)?;
//! # let person = Signature {
//! #     name: b"A. Writer".to_vec(),
//! #     email: b"writer@example.com".to_vec(),
//! #     seconds: 1_700_000_000,
//! #     offset_minutes: 0,
//! # };
//! # let loose = source.loose_objects();
//! # let commit = loose.write_commit(&Commit::new(CommitFields {
//! #     tree: loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?,
//! #     parents: vec![],
//! #     author: person.clone(),
//! #     committer: person.clone(),
//! #     extra_headers: vec![],
//! #     message: b"Initial commit\n".to_vec(),
//! # })?)?;
//! # let main = RefName::new("refs/heads/main")?;
//! # source.references()?.update_without_reflog(&main, Target::Direct(commit), Expected::Absent)?;
//! # let destination_path = directory.path().join("remote.git");
//! # let destination = Repository::init(ObjectFormat::Sha1, destination_path, InitKind::Bare)?;
//! # let objects = source.objects(PackLimits::default())?;
//! # let command = PushCommand::new(main.clone(), None, commit).with_force(ForcePolicy::FastForwardOnly);
//! let cancel = AtomicBool::new(false);
//! let prepared = PreparedPush::new(
//!     &objects, vec![command], &[], PushLimits::default(), &cancel,
//! )?;
//! let control = TransportControl::new(&cancel).with_deadline(Some(Instant::now()));
//! let result = send_local(
//!     destination.git_dir(),
//!     girt::push::LocalPushRequest::new(&prepared),
//!     control,
//!     |_| {}
//! );
//! assert!(matches!(result, Err(PushError::NotSent(PushFailure::Deadline))));
//! assert_eq!(destination.references()?.read(&main)?, None);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub mod ssh;

mod control;
mod protocol;
pub use protocol::RequestedProtocol;
#[cfg(feature = "http")]
pub mod http;
pub use control::TransportControl;
pub(crate) use control::{Interruption, interruption};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod process;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg(test)]
pub(crate) use process::Server;
