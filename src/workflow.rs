//! Clone repositories, fetch from, push to and query configured remotes.
//!
//! Most applications that talk to Git remotes want the behavior a Git user already expects: name a
//! remote such as `origin`, and let the repository's configuration decide the URL, the transport,
//! the credentials and the proxy. This module provides synchronous transfer functions:
//!
//! - [`clone()`] creates a repository and checks out its default branch; [`clone_with`] selects
//!   policy.
//! - [`fetch`] downloads objects and updates remote-tracking references and tags.
//! - [`push`] conditionally updates references on the remote.
//! - [`remote_head`] asks which branch the remote's `HEAD` names, as a clone does.
//!
//! Fetch, push and remote-head queries take the [`Repository`] explicitly, an [`Environment`]
//! snapshot instead of reading the process environment, and a [`TransferCallbacks`] implementation
//! for progress and credential prompts. They return typed reports, never printed text.
//!
//! # Quick start
//!
//! Create a repository, add a remote to its configuration, then fetch from it. In this example
//! `origin` is a local repository path, so the example runs without a network; an `https://` or
//! `ssh://` URL takes the same code path once the `http` or `ssh` feature is enabled.
//!
//! ```
//! use girt::refs::{RefName, Target};
//! use girt::remote::RemoteConfig;
//! use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! // `directory` already holds `upstream.git`, a repository whose `main` branch points to the
//! // commit `main` (setup hidden).
//! # let (directory, _, _, main, _) = include!("doctest/remote.rs");
//! let path = directory.path().join("project");
//! Repository::init(ObjectFormat::Sha1, &path, InitKind::Worktree)?;
//!
//! // Record the remote in the repository's own config file, as `git remote add` would.
//! let mut edit = Repository::open(&path)?.edit_config(1 << 20)?;
//! RemoteConfig::new(edit.document_mut()).add(
//!     b"origin",
//!     b"../upstream.git",
//!     &[b"+refs/heads/*:refs/remotes/origin/*"],
//! )?;
//! edit.commit()?;
//!
//! // A `Repository` holds a configuration snapshot, so reopen it to see the new remote.
//! let repo = Repository::open(&path)?;
//! let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
//! // Applications usually pass `Environment::from_process()`. An empty environment keeps this
//! // example independent of the caller's settings, and makes the relative URL resolve against
//! // the working tree.
//! let outcome = workflow::fetch(
//!     &repo,
//!     "origin",
//!     &options,
//!     &Environment::default(),
//!     &mut NoCallbacks,
//! )?;
//!
//! let tracking = RefName::new("refs/remotes/origin/main")?;
//! assert_eq!(outcome.updated.len(), 1);
//! assert_eq!(outcome.updated[0].name, tracking);
//! assert_eq!(
//!     repo.references()?.read(&tracking)?,
//!     Some(Target::Direct(main))
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Choosing between `workflow` and the core transfer APIs
//!
//! girt has two layers for moving objects between repositories. This module is the convenience
//! layer. The core layer, [`crate::fetch`], [`crate::push`] and [`crate::clone`], makes every
//! decision explicit: which endpoint, which resource limits, which credentials, when to install
//! objects and when to publish references. Use this module when:
//!
//! - the remote is named in the repository's configuration and should be resolved as Git resolves
//!   it;
//! - the remote is trusted the way a Git user trusts their own remotes (see [Limits and
//!   trust](#limits-and-trust));
//! - the caller can block a thread for the whole transfer and does not need to cancel it.
//!
//! Use the core APIs instead when the endpoint comes from somewhere other than configuration, when
//! the peer is untrusted and transfers must be bounded, when the caller needs cancellation or a
//! deadline, or when downloads should be validated on a separate worker pool. The core APIs also
//! cover operations this module does not, such as retaining a fetched pack against repacking while
//! the caller checks it before publishing references, or publishing shallow boundaries under caller
//! control. Depth-limited fetches themselves are supported here through
//! [`FetchOptions::with_depth`].
//!
//! # What a configured transfer does
//!
//! Configured transfers choose a transport and authenticate the same way:
//!
//! 1. **Resolve the remote.** The named remote's `url` (or, for [`push`], its first `pushurl`) is
//!    read from [`Repository::config`] and rewritten by `url.<base>.insteadOf` and `pushInsteadOf`.
//!    `protocol.allow`, `protocol.<name>.allow` and `GIT_ALLOW_PROTOCOL` decide whether the
//!    resulting transport may be used. See [`crate::remote::Remote::fetch_destination`] and
//!    [`crate::remote::Remote::push_destinations`].
//! 2. **Choose a transport** from the URL. Local paths and `file://` URLs use girt's native local transfer,
//!    which reads and writes the other repository's storage directly and never starts Git. `http://`
//!    and `https://` need the `http` feature; `ssh://` and `user@host:path` need the `ssh` feature
//!    on macOS or Linux. Other schemes, such as `git://` and `remote-helper::url`, return
//!    [`TransferError::UnsupportedTransport`].
//! 3. **Authenticate if asked.** An HTTP server that answers `401 Unauthorized` gets one retry with
//!    a credential. Configured `credential.helper` programs are asked first; Git's `store` helper
//!    is answered natively by [`crate::remote::CredentialStore`]. If no helper supplies a value,
//!    [`TransferCallbacks::credential`] is asked for the username and password. A credential the
//!    server accepts is reported to the helpers for storage; a rejected one is reported for
//!    removal. SSH authentication is left to OpenSSH and the user's SSH configuration, agent and
//!    `SSH_ASKPASS` program, all taken from the [`Environment`].
//!
//! [`remote_head`] then reads the reference advertisement and stops. [`fetch`] continues:
//!
//! 4. **Negotiate.** Local reference tips are offered as history the repository already has, so the
//!    server can leave those objects out. HTTP and SSH ask for the upload-pack protocol named by
//!    `protocol.version` (v2 when unset, as Git does) and accept a v0 or v1 answer.
//! 5. **Validate and install.** Every received object's framing, delta chain and identity are
//!    checked, and the new history of every selected tip, down to history the repository already
//!    has, is walked to check that it is complete and well-formed, before the pack is installed.
//! 6. **Publish.** First every planned change is checked against its refspec's rules, such as
//!    fast-forwarding; if any check fails, no reference changes. Then stale references are pruned
//!    in one transaction and the remaining references updated in a second, each conditional on the
//!    values seen before the transfer. A reference another process moved meanwhile is never
//!    overwritten, but that conflict can be detected after pruning has already happened, and an I/O
//!    failure can leave some references updated. The error reports what was published.
//!
//! [`push`] instead reads the remote's advertisement, sends a pack without the history the remote
//! already has, and lets the remote check each command's expected value. Commands succeed or fail
//! independently. Afterwards it updates the matching local remote-tracking references
//! unconditionally, as a best effort that a later fetch corrects.
//!
//! # Concurrency
//!
//! Other processes may update references in either repository during a transfer: fetch publishes
//! only if its destinations still have the values it started from, and the remote checks every
//! push command the same way. Reference locks do not protect everything, though. From the start
//! of a [`fetch`] until it returns, the caller must keep these from running in the local
//! repository, and for a [`push`] to a local path, in the destination too:
//!
//! - garbage collection or pruning, which could delete objects the transfer depends on;
//! - checkout, or changes to `HEAD` and which branch a worktree has checked out;
//! - adding or removing linked worktrees.
//!
//! [`crate::fetch::FetchRequest`] and [`crate::push::send_local`] document the full contract.
//!
//! # Limits and trust
//!
//! A Git client trusts the remotes its user configured: `git fetch` imposes no limit on how large a
//! pack, an object or an advertisement may be. This module follows Git and uses
//! [`FetchLimits::trusted`], [`crate::push::PushLimits::trusted`] and
//! [`crate::PackLimits::trusted`]. Validation is never relaxed. A trusted transfer still rejects
//! malformed, corrupt or disconnected data. What trust removes is the ceiling on how much
//! *well-formed* work a peer can ask for. A hostile server could send a multi-gigabyte pack or a
//! deeply nested delta chain, and the transfer would only stop when it ran out of disk, memory or
//! time.
//!
//! Clone instead uses bounded default limits; [`CloneOptions`] makes its bounds explicit.
//!
//! When the remote is chosen by someone other than the local user, for example a service that
//! fetches URLs submitted by its users, do not use this module. Call the core APIs with
//! [`FetchLimits::default`], which bounds wire bytes, pack size, object count, per-object size,
//! delta depth and decoding work, and set a deadline with
//! [`TransportControl::with_deadline`](crate::transport::TransportControl::with_deadline).
//! [`crate::fetch::FetchLimits`] documents each bound and the defaults.
//!
//! # Threads, runtimes and cancellation
//!
//! Every function blocks the calling thread until the transfer finishes. HTTP and SSH transfers
//! create a private single-threaded Tokio runtime for the duration of the call, so these functions
//! must not be called from a thread that is already running a Tokio runtime: Tokio panics when a
//! runtime is started inside another. From async code, move the call to a blocking thread with
//! `tokio::task::spawn_blocking`. Local transfers start no runtime.
//!
//! Callbacks run on the calling thread, in order. A transfer cannot be cancelled and has no
//! deadline; a stalled HTTP server or SSH connection blocks until the operating system or the peer
//! gives up. Applications that need either use the core APIs, which take a [`TransportControl`].
//!
//! # Errors
//!
//! Fetch, push and remote-head queries return [`TransferError`]. Its variants say how far the
//! operation got:
//!
//! | Variant | Local changes | What to do |
//! | --- | --- | --- |
//! | [`NoSuchRemote`](TransferError::NoSuchRemote) | none | Check the name or add the remote. |
//! | [`Configuration`](TransferError::Configuration) | none | Report it; the user's config is invalid or unsupported. |
//! | [`UnsupportedTransport`](TransferError::UnsupportedTransport) | none | Enable the feature or use another URL. |
//! | [`Authentication`](TransferError::Authentication) | none | Ask for a different credential. |
//! | [`Fetch`](TransferError::Fetch) | none | Inspect the cause; retrying is safe. |
//! | [`FetchFinish`](TransferError::FetchFinish) | objects, shallow boundaries or some references, depending on the phase | Inspect the report; fetching again is safe. |
//! | [`Push`](TransferError::Push) | none locally | If [`Uncertain`](crate::push::PushError::Uncertain), read the remote's references before retrying. |
//! | [`Local`](TransferError::Local) | none | Reading this repository failed. |
//!
//! Clone returns [`CloneError`], preserving destination creation and checkout reports as well as
//! transfer errors.
//!
//! A successful [`push`] can still contain rejected references: check
//! [`PushReport::all_succeeded`](crate::push::PushReport::all_succeeded) or each
//! [`RefStatus`](crate::push::RefStatus).
//!
//! # How jj uses this module
//!
//! [jj](https://github.com/jj-vcs/jj)'s experimental girt backend uses this module for every Git
//! remote operation. Its adapter is a useful model for other version-control front ends:
//!
//! - It captures [`Environment::from_process`] once and layers jj's own overrides on top with
//!   [`Environment::with`], so tests and subprocesses see exactly the variables jj chose.
//! - `jj git fetch` builds [`FetchOptions`] from the user's branch patterns, including negative
//!   `^refs/heads/…` refspecs, and always enables [`FetchOptions::with_prune`]. It passes the
//!   requested [`FetchOptions::with_depth`], records reflog entries with
//!   [`FetchOptions::with_reflog`], and keeps per-remote tags in a
//!   [`FetchOptions::with_tag_namespace`]. Branches named exactly but absent on the remote arrive
//!   in [`FetchOutcome::missing_sources`] and are logged rather than failing the fetch.
//! - After a fetch, jj reopens its [`Repository`] and object store. An open
//!   [`Objects`](crate::Objects) reader keeps the pack list it started with, so it cannot see the
//!   pack the fetch just installed.
//! - jj finds a remote's default branch with [`remote_head`]. Only [`RemoteHead::Symbolic`] and
//!   [`RemoteHead::Inferred`] name one; every other answer means "no default branch".
//! - `jj git push` sends one [`PushCommand`] per bookmark, with the remote value jj last saw as
//!   `expected` and [`ForcePolicy::Allow`](crate::push::ForcePolicy::Allow). That is Git's
//!   `--force-with-lease`: jj may move a bookmark sideways, but only if nobody else moved it first.
//!   A rejection with
//!   [`RejectionOrigin::ExpectedValue`](crate::push::RejectionOrigin::ExpectedValue) becomes jj's
//!   "stale info" message.
//! - Its [`TransferCallbacks`] splits [`TransferCallbacks::remote_message`] bytes into lines at
//!   `\r` and `\n`, parses `Counting objects:` lines into a progress bar, forwards SSH diagnostics
//!   from [`TransferCallbacks::transport_message`] to the terminal, and answers
//!   [`TransferCallbacks::credential`] with jj's own prompt.
//!
//! # Platform and feature support
//!
//! Local paths always work. HTTP(S) needs the `http` feature. SSH needs the `ssh` feature and runs
//! the system OpenSSH client, on macOS and Linux only. See [`crate::transport`] for each
//! transport's configuration support and limitations.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::fetch::{
    FetchLimits, FetchRequest, FetchUpdateKind, FetchUpdateLimits, FetchWorkflowError,
    KnownHistory, RemoteHead,
};
use crate::push::{PreparedPush, PushCommand, PushFailure, PushLimits, PushReport};
use crate::refs::{RefName, Reflog};
use crate::remote::{Destination, Direction, Protocol, ProtocolEnvironment, Refspecs, Remote};
use crate::transport::TransportControl;
use crate::{ObjectId, Repository};

mod clone;
pub use clone::{CloneError, CloneOptions, CloneOutcome, clone, clone_with};

/// Asks a configured remote which branch its `HEAD` names, without downloading objects.
///
/// This is how a client learns a remote's default branch, for example to choose what to check
/// out after a clone or which bookmark to track. Only the reference advertisement is read: no
/// objects are transferred and the local repository is not changed. The remote is resolved,
/// authenticated and reached exactly as by [`fetch`].
///
/// The answer is a snapshot. The remote's `HEAD` can change before any later fetch, so don't use
/// it as a precondition for updating references.
///
/// # Interpreting the answer
///
/// Servers that send a symbolic `HEAD` hint (all modern Git servers, and every local repository)
/// produce [`RemoteHead::Symbolic`], or [`RemoteHead::Unborn`] for an empty repository whose
/// default branch has no commits yet. Older servers advertise only `HEAD`'s object ID; girt then
/// reports [`RemoteHead::Inferred`] when exactly one branch has that ID, and
/// [`RemoteHead::Ambiguous`] or [`RemoteHead::Detached`] otherwise. It never guesses.
///
/// # Examples
///
/// ```
/// use girt::fetch::RemoteHead;
/// use girt::workflow::{self, Environment, NoCallbacks};
/// # let (_directory, _, repo, main, _) = include!("doctest/remote.rs");
///
/// // `repo` has a remote `origin` whose `HEAD` names `main`.
/// let head = workflow::remote_head(&repo, "origin", &Environment::default(), &mut NoCallbacks)?;
/// let default_branch = match head {
///     RemoteHead::Symbolic { branch, id } => {
///         assert_eq!(id, main);
///         Some(branch)
///     }
///     RemoteHead::Inferred { branch, .. } => Some(branch),
///     // An empty remote, a detached HEAD or an ambiguous answer has no default branch.
///     _ => None,
/// };
/// assert_eq!(
///     default_branch,
///     Some(girt::refs::RefName::new("refs/heads/main")?)
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`TransferError`] when the remote is missing or misconfigured, its transport is
/// unsupported, authentication fails, or the advertisement cannot be read. A failed query changes
/// nothing.
pub fn remote_head(
    repository: &Repository,
    remote_name: &str,
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<RemoteHead, TransferError> {
    repository.remote_head(remote_name, environment, callbacks)
}

/// Downloads objects from a configured remote and updates local references to match.
///
/// The refspecs in `options` choose which remote references to fetch and where to record them,
/// exactly as on the `git fetch` command line; the remote's configured `fetch` refspecs are not
/// added. A typical call passes the remote's configured refspecs, such as
/// `+refs/heads/*:refs/remotes/origin/*`.
///
/// Objects are validated and installed before any reference moves. Every planned change is then
/// checked against its refspec's rules; if one fails, no reference changes. Pruned references
/// are deleted in one transaction and the other changes applied in a second, each requiring the
/// destination to still have the value it had when the fetch began, so a reference moved by
/// another process during the transfer is never overwritten. Because the transactions are
/// separate, a conflict in the second can be reported after pruning has happened.
///
/// # What is and isn't updated
///
/// Destinations must be remote-tracking references (`refs/remotes/…`), tags (`refs/tags/…`), or
/// references below a namespace allowed with [`FetchOptions::with_tag_namespace`]. Local branches
/// are never fetch destinations, so a fetch cannot move the branch a working tree has checked out.
/// Without a `+` prefix, a remote-tracking reference whose old and new values are both commits
/// may only fast-forward, and an existing tag may not change; either violation fails the fetch
/// without updating any reference. A remote-tracking reference pointing to another kind of object,
/// such as a tree, can be replaced without `+`.
///
/// Unlike `git fetch`, no `FETCH_HEAD` is written and tags pointing into fetched history are not
/// followed automatically: name the tags you want with a refspec such as
/// `refs/tags/*:refs/tags/*`. Exact (non-glob) sources the remote doesn't have are reported in
/// [`FetchOutcome::missing_sources`] rather than failing the fetch.
///
/// Afterwards, like `git fetch`, this runs Git's automatic pack-refs task (see
/// [`Repository::auto_pack_references`]). A failure there does not fail the fetch.
///
/// The repository's open [`Objects`](crate::Objects) readers keep their earlier pack list. Open a
/// new reader, or reopen the repository, to read the fetched objects.
///
/// # Examples
///
/// Fetch every branch of `origin` into `refs/remotes/origin/`:
///
/// ```
/// use girt::refs::{RefName, Target};
/// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
/// # let (_directory, _, repo, main, _) = include!("doctest/remote.rs");
///
/// let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
/// let outcome = workflow::fetch(
///     &repo,
///     "origin",
///     &options,
///     &Environment::default(),
///     &mut NoCallbacks,
/// )?;
/// for update in &outcome.updated {
///     // Reference names are bytes, like Git's; they need not be UTF-8.
///     let name = String::from_utf8_lossy(update.name.as_bytes());
///     println!("{name}: {:?} -> {:?}", update.old, update.new);
/// }
///
/// let tracking = RefName::new("refs/remotes/origin/main")?;
/// assert_eq!(
///     repo.references()?.read(&tracking)?,
///     Some(Target::Direct(main))
/// );
///
/// // Nothing changed upstream, so a second fetch transfers nothing and updates nothing.
/// let again = workflow::fetch(
///     &repo,
///     "origin",
///     &options,
///     &Environment::default(),
///     &mut NoCallbacks,
/// )?;
/// assert!(again.updated.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`TransferError`]. Every variant except [`TransferError::FetchFinish`] means no local
/// change was made. `FetchFinish` means the pack was received and validated, but installing it or
/// publishing the results failed partway. Its [`FetchFinishError`](crate::fetch::FetchFinishError)
/// says how far it got: `report.installed` is set only if the pack was installed,
/// `report.shallow_published` if `.git/shallow` changed, and the failure carries any partial
/// reference effects. Fetching again is safe in every case.
pub fn fetch(
    repository: &Repository,
    remote_name: &str,
    options: &FetchOptions,
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<FetchOutcome, TransferError> {
    repository.fetch(remote_name, options, environment, callbacks)
}

/// Sends objects to a configured remote and conditionally updates its references.
///
/// Each [`PushCommand`] names a remote reference, the value the caller believes it has
/// (`expected`, or `None` for "must not exist"), and the new value (the null ID deletes it). The
/// remote applies a command only if the reference still has the expected value. This is Git's
/// `--force-with-lease` with an explicit lease, and it is always on: girt has no unconditional
/// push. [`ForcePolicy`](crate::push::ForcePolicy) separately decides whether a branch may move
/// to a commit that does not descend from its old value.
///
/// Commands succeed or fail independently. A successful return means the remote answered, not
/// that every update was applied; check [`PushReport::all_succeeded`] or each
/// [`RefStatus`](crate::push::RefStatus).
///
/// Like `git push`, the pack omits history reachable from anything the remote advertises, so
/// creating a branch sends only the commits the remote lacks. The pack is prepared after the
/// advertisement is read, while the connection is open. If a local remote's references change
/// during preparation, the push is prepared again once against the new references.
///
/// After a successful update, the matching remote-tracking reference (found through the remote's
/// `fetch` refspecs) is updated locally as `git push` does. Failures there are ignored; the next
/// fetch corrects them.
///
/// Only the remote's first push URL is used. Git pushes to every configured `pushurl`; to do the
/// same, configure one remote per URL. Atomic and signed pushes are unsupported. A local
/// destination's `receive.denyCurrentBranch` is honored as Git honors it, so by default a branch
/// checked out in the destination's working tree is refused; the `updateInstead` value is
/// unsupported and fails the push before anything is sent.
///
/// # Examples
///
/// Publish a new branch on `origin`. `expected: None` means the push is rejected if someone else
/// created the branch first with a different commit:
///
/// ```
/// use girt::push::{ForcePolicy, PushCommand};
/// use girt::refs::{RefName, Target};
/// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks, PushOptions};
/// # let (_directory, upstream, repo, main, person) = include!("doctest/remote.rs");
/// let environment = Environment::default();
///
/// // Fetch first, so the commit to publish exists locally.
/// let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
/// workflow::fetch(&repo, "origin", &options, &environment, &mut NoCallbacks)?;
///
/// let topic = RefName::new("refs/heads/topic")?;
/// let command =
///     PushCommand::new(topic.clone(), None, main).with_force(ForcePolicy::FastForwardOnly);
/// let report = workflow::push(
///     &repo,
///     "origin",
///     vec![command],
///     &PushOptions::default().with_identity(person),
///     &environment,
///     &mut NoCallbacks,
/// )?;
/// assert!(report.all_succeeded(), "rejected: {report:?}");
/// assert_eq!(
///     upstream.references()?.read(&topic)?,
///     Some(Target::Direct(main))
/// );
///
/// // The local remote-tracking reference now records the pushed value.
/// let tracking = RefName::new("refs/remotes/origin/topic")?;
/// assert_eq!(
///     repo.references()?.read(&tracking)?,
///     Some(Target::Direct(main))
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// A stale expectation is reported per reference rather than as an error:
///
/// ```
/// use girt::push::{ForcePolicy, PushCommand, RejectionOrigin, Status};
/// use girt::refs::RefName;
/// use girt::workflow::{self, Environment, NoCallbacks, PushOptions};
/// use girt::{ObjectFormat, ObjectId, ObjectKind};
/// # let (_directory, upstream, repo, main, _) = include!("doctest/remote.rs");
/// # let environment = Environment::default();
///
/// // Try to delete `main` on `origin`, believing it points to a commit it doesn't point to.
/// let believed = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"an outdated view");
/// let delete = ObjectId::null(ObjectFormat::Sha1);
/// let command = PushCommand::new(RefName::new("refs/heads/main")?, Some(believed), delete)
///     .with_force(ForcePolicy::Allow);
/// let report = workflow::push(
///     &repo,
///     "origin",
///     vec![command],
///     &PushOptions::default(),
///     &environment,
///     &mut NoCallbacks,
/// )?;
/// assert!(!report.all_succeeded());
/// assert!(matches!(report.refs[0].status, Some(Status::Rejected(_))));
/// assert_eq!(
///     report.refs[0].rejection_origin,
///     Some(RejectionOrigin::ExpectedValue)
/// );
/// # assert!(upstream.references()?.read(&RefName::new("refs/heads/main")?)?.is_some());
/// # let _ = main;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`TransferError::Push`] when the push could not complete. The boxed
/// [`PushError`](crate::push::PushError) says whether the remote may have been changed:
/// [`NotSent`](crate::push::PushError::NotSent) guarantees no command reached it, while
/// [`Uncertain`](crate::push::PushError::Uncertain) means some updates may have been applied.
/// After an uncertain failure, read the remote's references (for example with a fetch) before
/// pushing again. Other variants fail before any data is sent.
pub fn push(
    repository: &Repository,
    remote_name: &str,
    commands: Vec<PushCommand>,
    options: &PushOptions,
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<PushReport, TransferError> {
    repository.push(remote_name, commands, options, environment, callbacks)
}

/// The environment variables and working directory a transfer may consult.
///
/// Git's transfer behavior depends on process state: `GIT_SSH_COMMAND` selects the SSH client,
/// `https_proxy` and `no_proxy` choose a proxy, `GIT_SSL_CAINFO` replaces trusted certificates,
/// `GIT_ALLOW_PROTOCOL` restricts transports, `HOME` and `XDG_CONFIG_HOME` locate the credential
/// store, and `PATH` finds `ssh` and `git-credential-*` helpers. The functions in this module never
/// read the process environment themselves. They consult only this snapshot, and child processes
/// (SSH and credential helpers) receive exactly these variables.
///
/// That makes behavior reproducible and testable: a test can pass an environment with a private
/// `HOME`, and a server can run transfers for several users with different settings in one
/// process. Most applications start from [`Environment::from_process`] and adjust it.
///
/// The [`Debug`] output lists variable names only, because values such as proxy URLs can contain
/// credentials.
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
///
/// use girt::workflow::Environment;
///
/// // Start from this process and route SSH through a specific key, as `GIT_SSH_COMMAND` would.
/// let environment = Environment::from_process()
///     .with("GIT_SSH_COMMAND", "ssh -i ~/.ssh/deploy_key")
///     .without("GIT_ALLOW_PROTOCOL");
/// assert_eq!(
///     environment.get("GIT_SSH_COMMAND"),
///     Some(OsStr::new("ssh -i ~/.ssh/deploy_key"))
/// );
/// assert_eq!(environment.get("GIT_ALLOW_PROTOCOL"), None);
///
/// // Or build a fully controlled environment, for example in a test.
/// let isolated: Environment = [("HOME", "/nonexistent"), ("PATH", "/usr/bin:/bin")]
///     .into_iter()
///     .collect();
/// assert_eq!(isolated.get("HOME"), Some(OsStr::new("/nonexistent")));
/// assert_eq!(isolated.get("USER"), None);
/// ```
#[derive(Clone, Default)]
pub struct Environment {
    vars: BTreeMap<OsString, OsString>,
    current_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Values can contain secrets such as tokens.
        f.debug_struct("Environment")
            .field("names", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Environment {
    /// Captures the current process's environment variables and working directory.
    ///
    /// This is the usual starting point; the result behaves as `git` would when started from this
    /// process. The snapshot is taken once: later changes to the process environment do not
    /// affect it. If the working directory cannot be read (for example because it was deleted),
    /// relative local remote paths resolve against the repository instead; see
    /// [`Self::with_current_dir`].
    pub fn from_process() -> Self {
        let mut environment: Self = std::env::vars_os().collect();
        environment.current_dir = std::env::current_dir().ok();
        environment
    }

    /// Sets the directory that relative local remote paths are resolved against.
    ///
    /// Git resolves a remote URL such as `../upstream.git` against its working directory. Without
    /// a current directory (for example an environment built with [`FromIterator`]), relative paths
    /// resolve against the repository's working tree, or its Git directory if it is bare. A
    /// relative `credential.helper=store --file` path resolves against this directory as well.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::workflow::{self, Environment, NoCallbacks};
    /// # use girt::remote::RemoteConfig;
    /// # let (directory, _, repo, main, _) = include!("doctest/remote.rs");
    /// # let mut edit = repo.edit_config(1 << 20)?;
    /// # RemoteConfig::new(edit.document_mut()).add(b"relative", b"upstream.git", &[])?;
    /// # edit.commit()?;
    /// # let repo = girt::Repository::open(directory.path().join("local"))?;
    ///
    /// // `repo` has a remote `relative` whose URL is the relative path `upstream.git`.
    /// let environment = Environment::default().with_current_dir(directory.path());
    /// let head = workflow::remote_head(&repo, "relative", &environment, &mut NoCallbacks)?;
    /// assert!(matches!(head, girt::fetch::RemoteHead::Symbolic { id, .. } if id == main));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(directory.into());
        self
    }

    /// Returns a variable's value, or `None` if it is not set.
    ///
    /// Names are compared exactly, including case, on every platform.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    /// Sets a variable, replacing any previous value, and returns `self` for chaining.
    pub fn with(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Removes a variable, if set, and returns `self` for chaining.
    ///
    /// Removing a variable makes the transfer behave as if it were never set, which is different
    /// from setting it to an empty value: an empty `GIT_ALLOW_PROTOCOL` allows no protocols.
    pub fn without(mut self, name: impl AsRef<OsStr>) -> Self {
        self.vars.remove(name.as_ref());
        self
    }

    /// The directory relative paths resolve against, if known.
    pub(crate) fn current_dir(&self) -> Option<&std::path::Path> {
        self.current_dir.as_deref()
    }

    fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.get(name)
            .map(|value| value.as_encoded_bytes().to_vec())
    }

    #[cfg(feature = "http")]
    fn string(&self, name: &str) -> Option<String> {
        self.get(name).and_then(OsStr::to_str).map(str::to_owned)
    }
}

impl<K: Into<OsString>, V: Into<OsString>> FromIterator<(K, V)> for Environment {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            vars: iter
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
            current_dir: None,
        }
    }
}

/// Which part of a credential [`TransferCallbacks::credential`] is asking for.
///
/// An HTTP transfer asks for the username first, unless a credential helper or the
/// `credential.username` setting already supplied one, then the password. A personal access token
/// is usually supplied as the password. The enum is non-exhaustive: return `None` for prompts the
/// application does not recognize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CredentialPrompt {
    /// The username for the URL.
    Username,
    /// The password or token for the URL.
    Password,
}

/// Receives progress and diagnostics during a transfer, and answers credential prompts.
///
/// A command-line client shows the remote's progress messages, SSH warnings and a password prompt;
/// a GUI or service needs the same information in its own form. Implement the methods you need:
/// every method has a default that ignores its input or declines to answer. Use [`NoCallbacks`]
/// when nothing should be shown and no prompt can be answered.
///
/// Callbacks run on the thread that called [`fetch`], [`push`] or [`remote_head`], one at a time
/// and in the order events occur. The transfer waits while a callback runs, so return promptly;
/// a credential prompt that waits for the user is expected to block. Callbacks cannot cancel the
/// transfer.
///
/// All message bytes come from the remote or a local process and are untrusted. They need not be
/// UTF-8 and may contain terminal control sequences; sanitize them before display.
///
/// # Examples
///
/// Show the remote's progress the way a terminal would, and answer prompts with a token, as a
/// non-interactive client might:
///
/// ```
/// use girt::workflow::{CredentialPrompt, TransferCallbacks};
///
/// #[derive(Default)]
/// struct Display {
///     /// Bytes of the line received so far.
///     pending: Vec<u8>,
///     /// The line currently being redrawn, if any.
///     progress: Option<String>,
///     /// Finished lines.
///     lines: Vec<String>,
///     token: Option<String>,
/// }
///
/// impl TransferCallbacks for Display {
///     fn remote_message(&mut self, message: &[u8]) {
///         // A call can hold several lines or part of one, so buffer bytes until a terminator
///         // before decoding: a split multi-byte character then stays intact.
///         for &byte in message {
///             if byte != b'\r' && byte != b'\n' {
///                 self.pending.push(byte);
///                 continue;
///             }
///             let line = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned();
///             if byte == b'\r' {
///                 // `\r` asks for the line to be redrawn by the next one.
///                 self.progress = Some(line);
///             } else {
///                 self.progress = None;
///                 self.lines.push(line);
///             }
///         }
///     }
///
///     fn credential(&mut self, _url: &str, prompt: CredentialPrompt) -> Option<Vec<u8>> {
///         match prompt {
///             // Many hosts accept any username alongside a token.
///             CredentialPrompt::Username => Some(b"x-access-token".to_vec()),
///             CredentialPrompt::Password => self.token.clone().map(String::into_bytes),
///             _ => None,
///         }
///     }
/// }
///
/// let mut display = Display {
///     token: Some("secret".into()),
///     ..Display::default()
/// };
/// display.remote_message(b"Counting objects:  50% (1/2)\rCounting obj");
/// assert_eq!(
///     display.progress.as_deref(),
///     Some("Counting objects:  50% (1/2)")
/// );
/// display.remote_message(b"ects: 100% (2/2), done.\n");
/// assert_eq!(display.lines, ["Counting objects: 100% (2/2), done."]);
/// assert_eq!(display.progress, None);
/// assert_eq!(
///     display.credential("https://example.com/repo.git", CredentialPrompt::Password),
///     Some(b"secret".to_vec()),
/// );
/// ```
pub trait TransferCallbacks {
    /// Receives a progress or informational message from the remote, as raw bytes.
    ///
    /// These are the `remote: …` lines `git` prints: object counting and compression progress,
    /// and messages from the server's hooks, such as a link to open a pull request. A message
    /// ending in `\r` is meant to be overwritten by the next one; `\n` ends a line. One call
    /// may carry several lines or part of one, so buffer until a terminator before parsing.
    fn remote_message(&mut self, _message: &[u8]) {}

    /// Receives counts of objects and deltas validated so far, for a local transfer.
    ///
    /// Validation checks every received object before installation, and is the longest local
    /// phase of a large fetch or push. Each call carries a cumulative snapshot; use it to draw a
    /// progress bar. Completion means validation finished, not that objects were installed or
    /// references updated. A transfer with no pack makes no calls, and counts may start over if
    /// a push is prepared again.
    ///
    /// Fetch reports this for native local, HTTP and SSH validation; native local push reports
    /// receiver validation. Remote-owned push validation is reported through
    /// [`Self::remote_message`] instead. A panic unwinds before received data is installed.
    fn validation_progress(&mut self, _progress: crate::fetch::ValidationProgress) {}

    /// Receives synchronous source traversal and pack construction counts for native local fetch.
    /// Completion precedes installation and reference publication.
    fn fetch_progress(&mut self, _progress: crate::fetch::LocalFetchProgress) {}

    /// Receives completed and total working-tree operations during clone checkout.
    /// Completion precedes index publication; inspect the checkout report for final effects.
    fn checkout_progress(&mut self, _done: u64, _total: u64) {}

    /// Receives synchronous source traversal and pack construction counts while preparing a push.
    /// Counts may restart on retry; completion does not imply upload or receiver acceptance.
    fn push_progress(&mut self, _progress: crate::push::PreparationProgress) {}

    /// Receives diagnostic output of a local transport process, as raw bytes.
    ///
    /// For SSH this is the `ssh` client's standard error: host-key warnings, "Permission denied
    /// (publickey)" and similar. Show it to the user when a transfer fails, since it usually
    /// explains why. Chunks are not aligned to lines.
    fn transport_message(&mut self, _message: &[u8]) {}

    /// Asks the user for part of a credential, after configured helpers didn't supply one.
    ///
    /// Called only for HTTP(S) remotes that answer `401 Unauthorized`. `url` identifies the
    /// remote for the prompt; it never contains a username or password. Return the bytes the user
    /// entered, or `None` to cancel, which fails the transfer with
    /// [`TransferError::Authentication`]. A credential the server accepts is passed to the
    /// configured `credential.helper` programs for storage.
    ///
    /// SSH never calls this: OpenSSH prompts through `SSH_ASKPASS` or its agent instead.
    fn credential(&mut self, _url: &str, _prompt: CredentialPrompt) -> Option<Vec<u8>> {
        None
    }
}

/// Callbacks that discard all messages and decline every credential prompt.
///
/// Use it for background transfers and tests. A remote that requires a password the configured
/// helpers cannot supply fails with [`TransferError::Authentication`].
///
/// # Examples
///
/// ```
/// use girt::workflow::{self, Environment, NoCallbacks};
/// # let (_directory, _, repo, _, _) = include!("doctest/remote.rs");
///
/// let head = workflow::remote_head(&repo, "origin", &Environment::default(), &mut NoCallbacks)?;
/// # let _ = head;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCallbacks;

impl TransferCallbacks for NoCallbacks {}

/// Which references [`fetch`] downloads and how it records them locally.
///
/// Start with [`FetchOptions::new`] and the refspecs to fetch, then opt into pruning, a depth
/// limit, reflog entries or extra tag namespaces. By default the fetch is a plain `git fetch
/// <remote> <refspecs>`: full history, no pruning and no reflog entries.
///
/// # Examples
///
/// Fetch the remote's branches and tags, delete remote-tracking references for branches that no
/// longer exist, and record a reflog entry for each change:
///
/// ```
/// use girt::workflow::FetchOptions;
/// # let person = girt::Signature {
/// #     name: b"A. Writer".to_vec(),
/// #     email: b"writer@example.com".to_vec(),
/// #     seconds: 1_700_000_000,
/// #     offset_minutes: 0,
/// # };
///
/// let options = FetchOptions::new([
///     "+refs/heads/*:refs/remotes/origin/*",
///     "refs/tags/*:refs/tags/*",
///     // Negative refspecs exclude matching sources, as in Git.
///     "^refs/heads/wip/*",
/// ])
/// .with_prune(true)
/// .with_reflog(person, "fetch origin");
/// # let _ = options;
/// ```
#[derive(Clone, Debug)]
pub struct FetchOptions {
    refspecs: Vec<Vec<u8>>,
    depth: Option<NonZeroU32>,
    prune: bool,
    tag_namespaces: Vec<RefName>,
    reflog: Option<(crate::Signature, Vec<u8>)>,
}

impl FetchOptions {
    /// Fetches with the given refspecs and no other options.
    ///
    /// Each refspec uses Git's syntax: `[+]<src>:<dst>` maps remote references to local ones, `*`
    /// matches one glob segment, a leading `+` permits non-fast-forward updates, and `^<src>`
    /// excludes sources. A refspec with no destination (`refs/heads/main`) downloads the objects
    /// without recording a reference. Refspecs are parsed when the fetch starts; an invalid one
    /// fails it with [`TransferError::Configuration`] before any network access.
    ///
    /// An empty list fetches nothing.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks, TransferError};
    /// # let (_directory, _, repo, _, _) = include!("doctest/remote.rs");
    ///
    /// // Two `*` in one side is not a valid refspec, so the fetch fails before connecting.
    /// let options = FetchOptions::new(["refs/heads/*/*:refs/remotes/origin/*"]);
    /// let result = workflow::fetch(
    ///     &repo,
    ///     "origin",
    ///     &options,
    ///     &Environment::default(),
    ///     &mut NoCallbacks,
    /// );
    /// assert!(matches!(result, Err(TransferError::Configuration(_))));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new<I, S>(refspecs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        Self {
            refspecs: refspecs
                .into_iter()
                .map(|refspec| refspec.as_ref().to_vec())
                .collect(),
            depth: None,
            prune: false,
            tag_namespaces: Vec::new(),
            reflog: None,
        }
    }

    /// Limits history to `depth` commits from each fetched tip, like `git fetch --depth`.
    ///
    /// The repository becomes shallow: the oldest fetched commits are recorded in
    /// `.git/shallow` as boundaries whose parents are absent. Fetching again with a larger depth
    /// deepens the history, and the boundaries move. `None` (the default) fetches complete
    /// history from a complete repository and keeps existing boundaries in a shallow one.
    ///
    /// Native local remotes support only a depth of one, with a single fetched branch, from a
    /// complete (not shallow) source.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU32;
    ///
    /// use girt::workflow::FetchOptions;
    ///
    /// let latest_only = FetchOptions::new(["+refs/heads/main:refs/remotes/origin/main"])
    ///     .with_depth(NonZeroU32::new(1));
    /// # let _ = latest_only;
    /// ```
    #[must_use]
    pub fn with_depth(mut self, depth: Option<NonZeroU32>) -> Self {
        self.depth = depth;
        self
    }

    /// Deletes local destinations whose remote sources no longer exist, like `git fetch --prune`.
    ///
    /// Pruning applies to remote-tracking references under `refs/remotes/` and to any
    /// [`Self::with_tag_namespace`], within the destinations the refspecs could produce. For a glob
    /// refspec, that is every existing reference the destination pattern matches; for an exact
    /// refspec, its one destination. Ordinary tags under `refs/tags/` are never pruned. Pruned
    /// references lose their reflogs, as in Git.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::refs::{Expected, RefName};
    /// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
    /// # let (_directory, upstream, repo, _, _) = include!("doctest/remote.rs");
    /// let environment = Environment::default();
    /// let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]).with_prune(true);
    /// workflow::fetch(&repo, "origin", &options, &environment, &mut NoCallbacks)?;
    ///
    /// // `main` is deleted upstream, so the next fetch deletes its remote-tracking reference.
    /// let main = RefName::new("refs/heads/main")?;
    /// let id = upstream.references()?.read(&main)?.expect("main exists");
    /// upstream
    ///     .references()?
    ///     .delete_without_reflog(&main, Expected::Value(id))?;
    ///
    /// let outcome = workflow::fetch(&repo, "origin", &options, &environment, &mut NoCallbacks)?;
    /// assert_eq!(outcome.updated.len(), 1);
    /// assert_eq!(outcome.updated[0].new, None, "pruned");
    /// assert_eq!(
    ///     repo.references()?
    ///         .read(&RefName::new("refs/remotes/origin/main")?)?,
    ///     None
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn with_prune(mut self, prune: bool) -> Self {
        self.prune = prune;
        self
    }

    /// Allows fetching tags into references below `namespace`, such as `refs/remote-tags/origin`.
    ///
    /// By default a fetch writes only remote-tracking references and `refs/tags/`. Keeping each
    /// remote's tags in its own namespace avoids conflicts when two remotes have a tag with the
    /// same name, which is how jj stores remote tags. References in such a namespace behave like
    /// remote-tracking references: a forced refspec may replace them, and [`Self::with_prune`]
    /// deletes them when the remote's tag disappears. Call this once per namespace.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::refs::RefName;
    /// use girt::workflow::FetchOptions;
    ///
    /// let options = FetchOptions::new(["+refs/tags/*:refs/remote-tags/origin/*"])
    ///     .with_tag_namespace(RefName::new("refs/remote-tags/origin")?)
    ///     .with_prune(true);
    /// # let _ = options;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn with_tag_namespace(mut self, namespace: RefName) -> Self {
        self.tag_namespaces.push(namespace);
        self
    }

    /// Appends a reflog entry by `committer` with `message` for each changed reference.
    ///
    /// Without this, no reflog entries are written and existing reflogs are left alone. `git
    /// fetch` uses messages such as `fetch origin`. Unchanged references get no entry.
    #[must_use]
    pub fn with_reflog(mut self, committer: crate::Signature, message: impl Into<Vec<u8>>) -> Self {
        self.reflog = Some((committer, message.into()));
        self
    }
}

/// One local reference that a [`fetch`] created, moved or deleted.
///
/// `old` and `new` describe the change: `old: None` is a new reference, `new: None` a pruned
/// one, and both set a moved one.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct FetchedRef {
    /// Local reference name.
    pub name: RefName,
    /// Previous value, if the reference existed.
    pub old: Option<ObjectId>,
    /// New value, or `None` if the reference was pruned.
    pub new: Option<ObjectId>,
}

/// What a successful [`fetch`] changed locally.
///
/// # Examples
///
/// Summarize a fetch the way `git fetch` reports it:
///
/// ```
/// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks};
/// # let (_directory, _, repo, _, _) = include!("doctest/remote.rs");
///
/// let options = FetchOptions::new([
///     "+refs/heads/*:refs/remotes/origin/*",
///     "+refs/heads/gone:refs/remotes/origin/gone",
/// ]);
/// let outcome = workflow::fetch(
///     &repo,
///     "origin",
///     &options,
///     &Environment::default(),
///     &mut NoCallbacks,
/// )?;
/// for change in &outcome.updated {
///     let name = String::from_utf8_lossy(change.name.as_bytes());
///     match (change.old, change.new) {
///         (None, Some(_)) => println!(" * [new]     {name}"),
///         (Some(_), None) => println!(" - [deleted] {name}"),
///         _ => println!("   [updated] {name}"),
///     }
/// }
/// for source in &outcome.missing_sources {
///     println!(
///         "couldn't find remote ref {}",
///         String::from_utf8_lossy(source)
///     );
/// }
/// assert_eq!(outcome.missing_sources, [b"refs/heads/gone".to_vec()]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct FetchOutcome {
    /// Local references that were created, updated or pruned, in refspec order. References that
    /// already had the fetched value are not listed.
    pub updated: Vec<FetchedRef>,
    /// Sources of exact (non-glob) refspecs that the remote doesn't have, as written in the
    /// refspec. Git fails the whole fetch in this case; girt fetches the rest and reports them
    /// here.
    pub missing_sources: Vec<Vec<u8>>,
}

/// Options for [`push`] beyond the commands themselves.
///
/// The default sends no push options, writes no reflog entries in a local destination, and uses
/// girt's bounded delta compression.
///
/// # Examples
///
/// ```
/// use girt::workflow::PushOptions;
/// # let person = girt::Signature {
/// #     name: b"A. Writer".to_vec(),
/// #     email: b"writer@example.com".to_vec(),
/// #     seconds: 1_700_000_000,
/// #     offset_minutes: 0,
/// # };
///
/// // Ask a Gerrit server to add a reviewer, and record who pushed in local reflogs.
/// let options = PushOptions::default()
///     .with_push_option("r=reviewer@example.com")
///     .with_identity(person);
/// # let _ = options;
/// ```
#[derive(Clone, Debug, Default)]
pub struct PushOptions {
    push_options: Vec<Vec<u8>>,
    identity: Option<crate::Signature>,
    compression: Option<crate::PackCompression>,
}

impl PushOptions {
    /// Selects compression for a configured push, replacing the default bounded delta policy.
    ///
    /// Both policies can reuse eligible stored compressed entries and internal deltas. Ordinary
    /// compression disables new internal/external delta search. Delta search respects its bounds;
    /// packs offering advertised receiver candidates additionally cap output depth at one.
    ///
    /// ```
    /// let _options =
    ///     girt::workflow::PushOptions::default().with_compression(girt::PackCompression::Ordinary);
    /// ```
    #[must_use]
    pub fn with_compression(mut self, compression: crate::PackCompression) -> Self {
        self.compression = Some(compression);
        self
    }

    /// Sets the identity for reflog entries this push writes.
    ///
    /// The identity is used for reflog entries in a native local destination, and for the local
    /// remote-tracking references a successful push updates (message `update by push`). Without
    /// it, those references are updated without reflog entries. Network servers write their own
    /// reflogs; this identity is not sent to them.
    #[must_use]
    pub fn with_identity(mut self, identity: crate::Signature) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Adds one server-specific value, like `git push --push-option`.
    ///
    /// Servers use push options for per-push settings such as Gerrit reviewers or GitLab merge
    /// request creation. Values are sent as given, in order. Push options require the server to
    /// advertise `push-options`; otherwise the push fails before any command is sent.
    #[must_use]
    pub fn with_push_option(mut self, option: impl Into<Vec<u8>>) -> Self {
        self.push_options.push(option.into());
        self
    }
}

/// How the remote's `HEAD` is configured, as advertised; an alias of [`RemoteHead`].
pub type RemoteHeadState = RemoteHead;

/// Why [`fetch`], [`push`] or [`remote_head`] failed, and how far it got.
///
/// The variants are ordered by how far the operation progressed. Only
/// [`FetchFinish`](Self::FetchFinish) and an uncertain [`Push`](Self::Push) can follow a change;
/// every other variant means nothing was changed locally or remotely. The
/// [module documentation](self#errors) has a table of recovery actions.
///
/// For SSH failures, also show the text received by [`TransferCallbacks::transport_message`]:
/// OpenSSH explains authentication and host-key problems there.
///
/// # Examples
///
/// ```
/// use girt::workflow::{self, Environment, FetchOptions, NoCallbacks, TransferError};
/// # let (_directory, _, repo, _, _) = include!("doctest/remote.rs");
///
/// let options = FetchOptions::new(["+refs/heads/*:refs/remotes/upstream/*"]);
/// match workflow::fetch(
///     &repo,
///     "upstream",
///     &options,
///     &Environment::default(),
///     &mut NoCallbacks,
/// ) {
///     Err(TransferError::NoSuchRemote(name)) => assert_eq!(name, "upstream"),
///     other => panic!("expected a missing remote, got {other:?}"),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// No remote has this name, or it has no URL for this direction.
    ///
    /// Holds the remote name.
    #[error("no such remote: {0}")]
    NoSuchRemote(String),
    /// The remote's configuration or the environment is invalid or unsupported.
    ///
    /// Examples: an unparsable refspec, a URL rewritten to a form girt cannot parse, a protocol
    /// refused by `protocol.allow`, an invalid `protocol.version`, an unreadable `GIT_SSL_CAINFO`
    /// file, or an HTTP setting girt does not implement. Holds a description. Nothing was sent.
    #[error("invalid remote configuration: {0}")]
    Configuration(String),
    /// The URL's transport is not available in this build or not supported at all.
    ///
    /// `http(s)://` URLs need the `http` feature and SSH URLs need the `ssh` feature on macOS or
    /// Linux. `git://` and remote helpers (`<transport>::<address>`) are never supported. Holds the
    /// remote name.
    #[error("unsupported transport for remote {0}")]
    UnsupportedTransport(String),
    /// An HTTP server asked for a credential and none could be obtained.
    ///
    /// Raised when credential helpers and [`TransferCallbacks::credential`] supply nothing or the
    /// user cancels the prompt. Holds the remote URL without any username or password. A
    /// credential the server then rejects is reported as an HTTP `401` status through
    /// [`Fetch`](Self::Fetch) or [`Push`](Self::Push). SSH authentication failures are reported
    /// by OpenSSH through those variants as well.
    #[error("authentication failed for {0}")]
    Authentication(String),
    /// The fetch failed before installing anything; the repository is unchanged.
    ///
    /// The boxed error distinguishes a refspec or destination policy problem
    /// ([`FetchWorkflowError::Plan`]) from a transport, protocol or validation failure
    /// ([`FetchWorkflowError::Transfer`]).
    #[error(transparent)]
    Fetch(Box<FetchWorkflowError>),
    /// The fetch validated the pack but failed while installing it or publishing the results.
    ///
    /// The boxed [`FetchFinishError`](crate::fetch::FetchFinishError) holds a report of the
    /// completed phases and the failure. Check `report.installed` before assuming objects were
    /// installed: a failed installation can leave an unindexed pack file behind. After
    /// installation, `.git/shallow` may already have changed (`report.shallow_published`), and a
    /// reference transaction failure carries the references it did publish. Nothing is rolled
    /// back; installed objects that nothing refers to are removed by later maintenance.
    #[error(transparent)]
    FetchFinish(Box<crate::fetch::FetchFinishError>),
    /// The push failed; the boxed [`PushError`](crate::push::PushError) says whether the remote
    /// may have changed.
    #[error(transparent)]
    Push(Box<crate::push::PushError>),
    /// Reading this repository's objects or references failed before the transfer began.
    #[error(transparent)]
    Local(Box<dyn std::error::Error + Send + Sync>),
}

impl TransferError {
    fn local(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Local(error.into())
    }
}

/// A resolved connection target.
enum Endpoint {
    Local(PathBuf),
    #[cfg(feature = "http")]
    Http(Box<Http>),
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    Ssh(Box<crate::transport::ssh::SshRemote>),
}

#[cfg(feature = "http")]
struct Http {
    settings_config: crate::Config,
    remote_name: Vec<u8>,
    destination: Destination,
    environment: crate::transport::http::HttpEnvironment,
    protocol: crate::transport::RequestedProtocol,
    helpers: Vec<crate::remote::CredentialHelper>,
    /// URL for prompts, without userinfo.
    display_url: String,
}

#[cfg(feature = "http")]
impl Http {
    fn remote(
        &self,
        credentials: Option<crate::remote::CredentialSession>,
    ) -> Result<crate::transport::http::HttpRemote, TransferError> {
        let settings = crate::transport::http::HttpSettings::resolve_for_remote(
            &self.settings_config,
            &self.remote_name,
            &self.destination,
            &self.environment,
        )
        .map_err(|error| TransferError::Configuration(error.to_string()))?;
        let remote = crate::transport::http::HttpRemote::configured(settings)
            .map_err(|error| TransferError::Configuration(error.to_string()))?
            .with_protocol(self.protocol);
        match credentials {
            Some(session) => remote
                .with_credentials(session)
                .map_err(|error| TransferError::Configuration(error.to_string())),
            None => Ok(remote),
        }
    }

    fn credentials(
        &self,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<crate::remote::CredentialSession, TransferError> {
        let url = url_parts(self.destination.bytes())
            .ok_or_else(|| TransferError::Configuration("invalid HTTP URL".into()))?;
        let context = crate::remote::CredentialContext {
            protocol: url.scheme.into_bytes(),
            host: url.authority.into_bytes(),
            path: Some(url.path.trim_matches('/').as_bytes().to_vec()),
            use_http_path: false,
        };
        let display_url = self.display_url.clone();
        let mut prompt = |prompt: crate::remote::Prompt| {
            let prompt = match prompt {
                crate::remote::Prompt::Username => CredentialPrompt::Username,
                crate::remote::Prompt::Password => CredentialPrompt::Password,
            };
            callbacks.credential(&display_url, prompt)
        };
        crate::remote::CredentialSession::fill(
            &self.settings_config,
            context,
            &self.helpers,
            Some(&mut prompt),
            &AtomicBool::new(false),
            None,
        )
        .map_err(|_| TransferError::Authentication(self.display_url.clone()))
    }
}

#[cfg(feature = "http")]
struct UrlParts {
    scheme: String,
    authority: String,
    path: String,
}

/// Splits `scheme://[user@]host[:port]/path` without percent decoding.
#[cfg(feature = "http")]
fn url_parts(bytes: &[u8]) -> Option<UrlParts> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (scheme, rest) = text.split_once("://")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(UrlParts {
        scheme: scheme.to_ascii_lowercase(),
        authority: authority.to_owned(),
        path: path.to_owned(),
    })
}

#[cfg(feature = "http")]
fn redacted_url(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match url_parts(bytes) {
        Some(parts) => format!("{}://{}{}", parts.scheme, parts.authority, parts.path),
        None => text.into_owned(),
    }
}

/// Quotes `value` for a POSIX shell.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Finds `program` in the environment's `PATH`.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
fn find_program(environment: &Environment, program: &str) -> Option<PathBuf> {
    let path = environment.get("PATH")?;
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Maps configured `credential.helper` values to helpers, following Git's rules: `!cmd` is a
/// shell snippet, an absolute path is a shell command, and anything else names
/// `git-credential-<name>` (with any arguments). `store`, optionally with `--file`, runs natively.
#[cfg(feature = "http")]
fn credential_helpers(
    config: &crate::Config,
    environment: &Environment,
) -> Vec<crate::remote::CredentialHelper> {
    let exec_dirs: Vec<PathBuf> = environment
        .get("GIT_EXEC_PATH")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/opt/homebrew/opt/git/libexec/git-core",
                "/usr/local/opt/git/libexec/git-core",
                "/usr/lib/git-core",
                "/usr/libexec/git-core",
                "/Library/Developer/CommandLineTools/usr/libexec/git-core",
                "/Applications/Xcode.app/Contents/Developer/usr/libexec/git-core",
            ]
            .map(PathBuf::from),
        )
        .collect();
    config
        .entries()
        .iter()
        .filter(|entry| {
            entry.section.eq_ignore_ascii_case(b"credential")
                && entry.name.eq_ignore_ascii_case(b"helper")
        })
        .filter_map(|entry| entry.value.as_deref())
        .filter(|value| !value.is_empty())
        .filter_map(|value| {
            let text = std::str::from_utf8(value).ok()?;
            if let Some(store) = native_store(text, environment) {
                return Some(crate::remote::CredentialHelper {
                    configured_name: value.to_vec(),
                    backend: crate::remote::CredentialBackend::Store(store),
                });
            }
            let command = if let Some(snippet) = text.strip_prefix('!') {
                snippet.to_owned()
            } else if Path::new(text.split_whitespace().next()?).is_absolute() {
                text.to_owned()
            } else {
                let (name, arguments) = text.split_once(' ').unwrap_or((text, ""));
                let program = format!("git-credential-{name}");
                let path = find_program(environment, &program).or_else(|| {
                    exec_dirs
                        .iter()
                        .map(|dir| dir.join(&program))
                        .find(|candidate| candidate.is_file())
                })?;
                format!("{} {arguments}", shell_quote(&path.to_string_lossy()))
            };
            Some(crate::remote::CredentialHelper {
                configured_name: value.to_vec(),
                backend: crate::remote::CredentialBackend::Program(
                    crate::remote::CredentialProgram {
                        executable: PathBuf::from("/bin/sh"),
                        arguments: vec![
                            "-c".into(),
                            format!("{command} \"$@\"").into(),
                            command.into(),
                        ],
                        inherit_environment: false,
                        environment: environment
                            .vars
                            .iter()
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect(),
                    },
                ),
            })
        })
        .collect()
}

/// Recognizes `store`, `store --file <path>` and `store --file=<path>`, which girt answers itself.
/// Other arguments, and paths containing shell syntax, keep using Git's program.
#[cfg(feature = "http")]
fn native_store(text: &str, environment: &Environment) -> Option<crate::remote::CredentialStore> {
    let mut words = text.split_whitespace();
    if words.next()? != "store" {
        return None;
    }
    let home = environment.get("HOME").map(PathBuf::from);
    let file = match (words.next(), words.next()) {
        (None, _) => None,
        (Some("--file"), Some(path)) => Some(path),
        (Some(option), None) => Some(option.strip_prefix("--file=")?),
        _ => return None,
    };
    if words.next().is_some() {
        return None;
    }
    let Some(file) = file else {
        let xdg = environment.get("XDG_CONFIG_HOME").map(PathBuf::from);
        return Some(crate::remote::CredentialStore::user_files(
            home.as_deref(),
            xdg.as_deref(),
        ));
    };
    // Anything the shell would interpret (quotes, expansions, redirections, separators) keeps
    // Git's program, so only plain path bytes are taken literally.
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"/._-+,:@%=~".contains(&byte);
    if !file.bytes().all(plain) {
        return None;
    }
    let path = match file.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None if file.starts_with('~') => return None,
        None => PathBuf::from(file),
    };
    let path = match &environment.current_dir {
        Some(directory) if path.is_relative() => directory.join(path),
        _ => path,
    };
    Some(crate::remote::CredentialStore::file(path))
}

#[cfg(feature = "http")]
fn http_environment(
    config: &crate::Config,
    destination: &Destination,
    environment: &Environment,
) -> Result<crate::transport::http::HttpEnvironment, TransferError> {
    let ca_path = match environment.get("GIT_SSL_CAINFO") {
        Some(path) => Some(PathBuf::from(path)),
        None => crate::transport::http::HttpSettings::configured_ca_info(config, destination)
            .map_err(|error| TransferError::Configuration(error.to_string()))?
            .map(|path| {
                let path = String::from_utf8_lossy(path).into_owned();
                match (path.strip_prefix("~/"), environment.get("HOME")) {
                    (Some(rest), Some(home)) => Path::new(home).join(rest),
                    _ => PathBuf::from(path),
                }
            }),
    };
    let ssl_ca_info = ca_path
        .map(|path| {
            std::fs::read(&path).map_err(|error| {
                TransferError::Configuration(format!("reading {}: {error}", path.display()))
            })
        })
        .transpose()?;
    let proxy_var = |names: &[&str]| names.iter().find_map(|name| environment.string(name));
    Ok(crate::transport::http::HttpEnvironment {
        ssl_ca_info,
        ssl_no_verify: environment
            .bytes("GIT_SSL_NO_VERIFY")
            .map(|value| crate::config::boolean(Some(&value)).unwrap_or(true)),
        proxy: proxy_var(&[
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "all_proxy",
            "ALL_PROXY",
        ]),
        no_proxy: proxy_var(&["no_proxy", "NO_PROXY"]),
        proxy_credentials: None,
        allow_insecure_tls: true,
    })
}

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
/// Reads `protocol.version` for an HTTP or SSH endpoint, defaulting to v2 like Git. Only
/// upload-pack sends the request; a server answering v0/v1 is accepted. Native local transfer never
/// reads it, so enabling a network feature cannot change local fetch or push.
fn requested_protocol(
    config: &crate::Config,
) -> Result<crate::transport::RequestedProtocol, TransferError> {
    match config.value("protocol", None, "version") {
        None => Ok(crate::transport::RequestedProtocol::V2),
        Some(value) => value
            .and_then(crate::transport::RequestedProtocol::from_config)
            .ok_or_else(|| TransferError::Configuration("invalid protocol.version".into())),
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
fn ssh_endpoint(
    config: &crate::Config,
    destination: &Destination,
    environment: &Environment,
) -> Result<crate::transport::ssh::SshRemote, TransferError> {
    use crate::transport::ssh::{ApprovedSshCommand, OpenSshOptions, SshRemote};
    // Git runs a configured command through the shell with the SSH arguments appended.
    let configured = environment
        .bytes("GIT_SSH_COMMAND")
        .or_else(|| {
            config
                .string("core", None, "sshcommand")
                .map(<[u8]>::to_vec)
        })
        .map(|command| (command.clone(), command))
        .or_else(|| {
            environment.bytes("GIT_SSH").map(|program| {
                let quoted = shell_quote(&String::from_utf8_lossy(&program));
                (program, quoted.into_bytes())
            })
        });
    let approved_command = configured.map(|(configured, command)| {
        let command = String::from_utf8_lossy(&command).into_owned();
        ApprovedSshCommand {
            configured,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".into(),
                format!("{command} \"$@\"").into(),
                command.into(),
            ],
        }
    });
    let default_executable =
        find_program(environment, "ssh").unwrap_or_else(|| PathBuf::from("/usr/bin/ssh"));
    // GIT_SSH_VARIANT and ssh.variant only matter for non-OpenSSH clients.
    let environment = environment
        .clone()
        .without("GIT_PROTOCOL")
        .without("GIT_SSH_VARIANT");
    SshRemote::openssh(
        config,
        destination,
        OpenSshOptions {
            default_executable,
            approved_command,
            environment: environment.vars,
        },
    )
    .map_err(|error| TransferError::Configuration(error.to_string()))
}

fn configured_endpoint(
    config: &crate::Config,
    remote: &Remote,
    remote_name: &str,
    direction: Direction,
    base: &Path,
    environment: &Environment,
) -> Result<(Endpoint, Destination), TransferError> {
    let protocol_environment =
        ProtocolEnvironment::from_environment(|name| environment.bytes(name))
            .map_err(|error| TransferError::Configuration(error.to_string()))?;
    let destination = match direction {
        Direction::Fetch => remote.fetch_destination(config, &protocol_environment),
        Direction::Push => remote
            .push_destinations(config, &protocol_environment)
            .and_then(|destinations| {
                destinations
                    .into_iter()
                    .next()
                    .ok_or(crate::remote::EndpointError::Missing)
            }),
    }
    .map_err(|error| match error {
        crate::remote::EndpointError::Missing => {
            TransferError::NoSuchRemote(remote_name.to_owned())
        }
        error => TransferError::Configuration(error.to_string()),
    })?;
    let endpoint = match destination.protocol() {
        Protocol::Local | Protocol::File => {
            let path = destination
                .local_path()
                .map_err(|error| TransferError::Configuration(error.to_string()))?;
            // Like Git, relative paths are relative to the working directory, falling back to
            // the working tree (or Git directory).
            let base = environment.current_dir.as_deref().unwrap_or(base);
            Endpoint::Local(base.join(path))
        }
        #[cfg(feature = "http")]
        Protocol::Http | Protocol::Https => Endpoint::Http(Box::new(Http {
            environment: http_environment(config, &destination, environment)?,
            protocol: requested_protocol(config)?,
            helpers: credential_helpers(config, environment),
            display_url: redacted_url(destination.bytes()),
            settings_config: config.clone(),
            remote_name: remote_name.as_bytes().to_vec(),
            destination: destination.clone(),
        })),
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Protocol::Ssh => Endpoint::Ssh(Box::new(
            ssh_endpoint(config, &destination, environment)?
                .with_protocol(requested_protocol(config)?),
        )),
        _ => return Err(TransferError::UnsupportedTransport(remote_name.to_owned())),
    };
    Ok((endpoint, destination))
}

impl Repository {
    fn endpoint(
        &self,
        remote_name: &str,
        direction: Direction,
        environment: &Environment,
    ) -> Result<(Endpoint, Destination), TransferError> {
        let config = self.config();
        let remote = Remote::find(config, remote_name.as_bytes())
            .map_err(|error| TransferError::Configuration(error.to_string()))?
            .ok_or_else(|| TransferError::NoSuchRemote(remote_name.to_owned()))?;
        configured_endpoint(
            config,
            &remote,
            remote_name,
            direction,
            self.worktree().unwrap_or(self.git_dir()),
            environment,
        )
    }

    fn remote_head(
        &self,
        remote_name: &str,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<RemoteHead, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Fetch, environment)?;
        let cancel = AtomicBool::new(false);
        let control = TransportControl::new(&cancel);
        let limits = FetchLimits::trusted();
        let discovery = match endpoint {
            Endpoint::Local(path) => crate::fetch::discover_local(path, limits, control),
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let result = runtime.block_on(crate::fetch::discover_http(
                    &http.remote(None)?,
                    limits,
                    control,
                ));
                if is_unauthorized_fetch(&result) {
                    let session = http.credentials(callbacks)?;
                    runtime.block_on(crate::fetch::discover_http(
                        &http.remote(Some(session))?,
                        limits,
                        control,
                    ))
                } else {
                    result
                }
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                runtime()?.block_on(crate::fetch::discover_ssh(&ssh, limits, control))
            }
        };
        let _ = &callbacks;
        discovery
            .map(|discovery| discovery.head)
            .map_err(|error| TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error))))
    }

    fn fetch(
        &self,
        remote_name: &str,
        options: &FetchOptions,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<FetchOutcome, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Fetch, environment)?;
        let objects = self
            .objects_with(crate::PackLimits::trusted())
            .map_err(TransferError::local)?;
        // One listing supplies the have tips, force authorization and the snapshot every
        // reference edit is conditional on.
        let listed = self
            .references()
            .map_err(TransferError::local)?
            .list()
            .map_err(TransferError::local)?;
        let tips: Vec<ObjectId> = listed
            .iter()
            .filter_map(|reference| match reference.target {
                crate::refs::Target::Direct(id) => Some(id),
                crate::refs::Target::Symbolic(_) => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let cancel = AtomicBool::new(false);
        let limits = FetchLimits::trusted();
        let known = if matches!(endpoint, Endpoint::Local(_)) {
            // Keep explicit reference roots as boundary evidence without a wire have-batch walk.
            KnownHistory::from_store_roots(objects, &tips, &cancel).map_err(|error| {
                TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error)))
            })?
        } else {
            KnownHistory::from_store(objects, &tips, limits, &cancel).map_err(|error| {
                TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error)))
            })?
        };
        let request = || self.fetch_request(options, listed.clone());
        let control = TransportControl::new(&cancel);
        #[cfg(feature = "http")]
        let mut on_message = |message: &[u8]| {
            callbacks.remote_message(message);
            ControlFlow::Continue(())
        };
        let ready = match endpoint {
            Endpoint::Local(path) => {
                let callbacks = std::cell::RefCell::new(callbacks);
                request()?.receive_local(
                    path,
                    &known,
                    limits,
                    control,
                    crate::fetch::LocalFetchObserver::default()
                        .with_messages(&mut |message| {
                            callbacks.borrow_mut().remote_message(message);
                            ControlFlow::Continue(())
                        })
                        .with_construction(&mut |event| {
                            callbacks.borrow_mut().fetch_progress(event)
                        })
                        .with_validation(&mut |event| {
                            callbacks.borrow_mut().validation_progress(event)
                        }),
                )
            }
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let known = std::sync::Arc::new(known);
                let receive = |remote: crate::transport::http::HttpRemote,
                               messages: &mut dyn FnMut(&[u8]) -> ControlFlow<()>|
                 -> Result<_, TransferError> {
                    let download = runtime.block_on(request()?.receive_http_with_progress(
                        &remote,
                        Some(known.clone()),
                        limits,
                        control,
                        |message| {
                            let _ = messages(message);
                        },
                    ));
                    Ok(download)
                };
                let mut download = receive(http.remote(None)?, &mut on_message)?;
                if matches!(
                    &download,
                    Err(FetchWorkflowError::Transfer(
                        crate::fetch::FetchError::Http(crate::transport::http::HttpError::Status(
                            401
                        ))
                    ))
                ) {
                    let session = http.credentials(callbacks)?;
                    download = receive(http.remote(Some(session))?, &mut |message| {
                        callbacks.remote_message(message);
                        ControlFlow::Continue(())
                    })?;
                }
                download.and_then(|download| {
                    download.validate_with_progress(
                        &cancel,
                        |_| ControlFlow::Continue(()),
                        |event| callbacks.validation_progress(event),
                    )
                })
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                let callbacks = std::cell::RefCell::new(&mut *callbacks);
                let download = runtime()?.block_on(request()?.receive_ssh_with_progress(
                    &ssh,
                    Some(std::sync::Arc::new(known)),
                    limits,
                    control,
                    |message| callbacks.borrow_mut().transport_message(message),
                    |message| callbacks.borrow_mut().remote_message(message),
                ));
                download.and_then(|download| {
                    download.validate_with_progress(
                        &cancel,
                        |_| ControlFlow::Continue(()),
                        |event| callbacks.borrow_mut().validation_progress(event),
                    )
                })
            }
        }
        .map_err(|error| TransferError::Fetch(Box::new(error)))?;
        let report = ready
            .finish(FetchUpdateLimits::trusted(), &cancel)
            .map_err(|error| TransferError::FetchFinish(Box::new(error)))?;
        // Like the automatic maintenance `git fetch` runs afterwards, packing never fails the
        // fetch: its references are already updated, and a held lock means another process is
        // maintaining them. A fresh snapshot includes the received pack, needed to peel new tags.
        #[cfg(test)]
        pack_tests::BEFORE_AUTO_PACK.with(|hook| hook.take().map(|hook| hook()));
        if let Ok(objects) = self.objects_with(crate::PackLimits::trusted()) {
            let _ = self.auto_pack_references(&objects);
        }
        Ok(FetchOutcome {
            updated: report
                .updates
                .iter()
                .filter(|update| {
                    !matches!(
                        update.kind,
                        FetchUpdateKind::SourceOnly | FetchUpdateKind::Unchanged
                    )
                })
                .filter_map(|update| {
                    Some(FetchedRef {
                        name: update.mapping.destination.clone()?,
                        old: update.previous,
                        new: update.mapping.source.as_ref().map(|source| source.id),
                    })
                })
                .collect(),
            missing_sources: report.missing,
        })
    }

    fn fetch_request(
        &self,
        options: &FetchOptions,
        listed: Vec<crate::refs::Reference>,
    ) -> Result<FetchRequest, TransferError> {
        let specs = Refspecs::parse(Direction::Fetch, options.refspecs.iter())
            .map_err(|error| TransferError::Configuration(error.to_string()))?;
        // Forced refspecs may replace any existing destination, as with `git fetch`.
        let authorized: BTreeSet<RefName> = listed
            .iter()
            .map(|reference| reference.name.clone())
            .collect();
        let reflog = match &options.reflog {
            None => Reflog::Preserve,
            Some((committer, message)) => Reflog::AppendIfChanged {
                committer: committer.clone(),
                message: message.clone(),
            },
        };
        let mut request =
            FetchRequest::prepare_listed(self.clone(), specs, authorized, reflog, listed)
                .map_err(|error| TransferError::Fetch(Box::new(error.into())))?;
        if options.prune {
            // Pruned references lose their reflogs, as in Git.
            request = request.with_prune();
            #[expect(clippy::expect_used, reason = "constant reference name is valid")]
            let namespaces = std::iter::once(RefName::new("refs/remotes").expect("valid name"))
                .chain(options.tag_namespaces.iter().cloned());
            for namespace in namespaces {
                request = request.with_reflog_for_update_kind(
                    namespace,
                    FetchUpdateKind::Prune,
                    Reflog::Delete,
                );
            }
        }
        for namespace in &options.tag_namespaces {
            request = request.with_tag_destination_namespace(namespace.clone());
        }
        if let Some(depth) = options.depth {
            request = request.with_depth(depth);
        }
        Ok(request)
    }

    fn push(
        &self,
        remote_name: &str,
        commands: Vec<PushCommand>,
        options: &PushOptions,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<PushReport, TransferError> {
        self.push_observed(
            remote_name,
            commands,
            options,
            environment,
            callbacks,
            &|| {},
        )
    }

    /// Runs [`Self::push`], calling `before_prepare` before each preparation attempt.
    fn push_observed(
        &self,
        remote_name: &str,
        commands: Vec<PushCommand>,
        options: &PushOptions,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
        before_prepare: &dyn Fn(),
    ) -> Result<PushReport, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Push, environment)?;
        let objects = self
            .objects_with(crate::PackLimits::trusted())
            .map_err(TransferError::local)?;
        let format = objects.object_format();
        let mut limits = PushLimits::trusted();
        if let Some(compression) = options.compression {
            limits.compression = compression;
        }
        let cancel = AtomicBool::new(false);
        let control = TransportControl::new(&cancel);
        // Each transport reads the receiver's advertisement before preparing. Like `git
        // send-pack`, preparation excludes the history of every advertised object (`roots`), so
        // the pack holds only what the receiver lacks.
        let callbacks = std::cell::RefCell::new(callbacks);
        // The source snapshot is needed only to prepare. Preparing takes it, so its mapped
        // indexes and cached delta bases are released before the receiver validates and installs
        // the pack, rather than holding both repositories' storage at once. A retry refills it.
        let source = std::cell::RefCell::new(Some(objects));
        let refill = || -> Result<(), TransferError> {
            let mut source = source.borrow_mut();
            if source.is_none() {
                *source = Some(
                    self.objects_with(crate::PackLimits::trusted())
                        .map_err(TransferError::local)?,
                );
            }
            Ok(())
        };
        let prepare = |roots: &[ObjectId]| -> Result<PreparedPush, PushFailure> {
            before_prepare();
            // Every transport prepares at most once per send, and each retry refills first.
            let objects = source
                .borrow_mut()
                .take()
                .ok_or(PushFailure::Unsupported("preparing one push attempt twice"))?;
            let prepared = PreparedPush::new_with_progress(
                &objects,
                commands.clone(),
                roots,
                limits,
                &cancel,
                |event| callbacks.borrow_mut().push_progress(event),
            )?;
            let prepared = if options.push_options.is_empty() {
                prepared
            } else {
                prepared.with_push_options(options.push_options.clone())?
            };
            Ok(prepared.with_progress())
        };
        #[cfg(any(
            feature = "http",
            all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
        ))]
        let names: Vec<RefName> = commands.iter().map(|c| c.name.clone()).collect();
        let result = match endpoint {
            Endpoint::Local(path) => {
                let send = || {
                    crate::push::send_local_preparing_observed(
                        &path,
                        format,
                        control,
                        crate::push::ReceiverPolicy {
                            config_inputs: &crate::config::ConfigInputs::default(),
                            identity: options.identity.as_ref(),
                        },
                        |advertisement| prepare(&advertisement.receiver_roots()?),
                        |event| callbacks.borrow_mut().validation_progress(event),
                    )
                };
                let mut result = send();
                // The destination's references changed while preparing: prepare once more against
                // the new ones. A second change is reported rather than retried.
                if is_knowledge_changed(&result) {
                    refill()?;
                    result = send();
                }
                // Receive hook output arrives with the report rather than live.
                if let Ok(report) = &result {
                    for message in &report.progress {
                        callbacks.borrow_mut().remote_message(message);
                    }
                }
                result
            }
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let send = |remote: &crate::transport::http::HttpRemote,
                            callbacks: &std::cell::RefCell<&mut dyn TransferCallbacks>| {
                    let advertised = std::cell::RefCell::new(BTreeMap::new());
                    let result = runtime.block_on(crate::push::send_http_preparing(
                        remote,
                        format,
                        limits,
                        control,
                        |advertisement| {
                            record_targets(advertisement, &names, &advertised);
                            prepare(&advertisement.receiver_roots())
                        },
                        |message| callbacks.borrow_mut().remote_message(message),
                    ));
                    result.map(|mut report| {
                        mark_up_to_date(&mut report, &advertised.into_inner());
                        report
                    })
                };
                let remote = http.remote(None)?;
                let mut result = send(&remote, &callbacks);
                if is_unauthorized_push(&result) {
                    let session = http.credentials(&mut **callbacks.borrow_mut())?;
                    let remote = http.remote(Some(session))?;
                    refill()?;
                    result = send(&remote, &callbacks);
                }
                result
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                let runtime = runtime()?;
                let advertised = std::cell::RefCell::new(BTreeMap::new());
                let result = runtime.block_on(crate::push::send_ssh_preparing(
                    &ssh,
                    format,
                    limits,
                    control,
                    |advertisement| {
                        record_targets(advertisement, &names, &advertised);
                        prepare(&advertisement.receiver_roots())
                    },
                    |message| callbacks.borrow_mut().transport_message(message),
                    |message| callbacks.borrow_mut().remote_message(message),
                ));
                result.map(|mut report| {
                    mark_up_to_date(&mut report, &advertised.into_inner());
                    report
                })
            }
        };
        let report = result.map_err(|error| TransferError::Push(Box::new(error)))?;
        self.update_tracking_refs(remote_name, &report, options.identity.as_ref());
        Ok(report)
    }

    /// Updates local remote-tracking references for successfully pushed references, as `git
    /// push` does, using the remote's fetch refspecs. Failures are ignored: the next fetch
    /// corrects them.
    fn update_tracking_refs(
        &self,
        remote_name: &str,
        report: &PushReport,
        identity: Option<&crate::Signature>,
    ) {
        let Ok(Some(remote)) =
            crate::remote::ConfiguredRemote::find(self.config(), remote_name.as_bytes())
        else {
            return;
        };
        let Ok(refs) = self.references() else {
            return;
        };
        let succeeded = report.unpack == Some(crate::push::Status::Ok);
        for status in &report.refs {
            if !succeeded || status.status != Some(crate::push::Status::Ok) {
                continue;
            }
            let Some(name) = tracking_ref(remote.fetch_refspecs(), status.command.name.as_bytes())
            else {
                continue;
            };
            let deleted = status.command.deletes();
            let edit = crate::refs::RefEdit {
                name,
                dereference: crate::refs::Dereference::Stored,
                target: (!deleted).then_some(crate::refs::Target::Direct(status.command.new)),
                expected: crate::refs::Expected::Any,
                reflog: match (deleted, identity) {
                    (true, _) => Reflog::Delete,
                    (false, Some(identity)) => Reflog::AppendIfChanged {
                        committer: identity.clone(),
                        message: b"update by push".to_vec(),
                    },
                    (false, None) => Reflog::Preserve,
                },
            };
            // Applied independently so one conflicting name doesn't block the others.
            let _ = refs.transaction(std::slice::from_ref(&edit));
        }
    }
}

/// Maps a remote reference name through fetch refspecs to its remote-tracking reference.
fn tracking_ref(refspecs: &[crate::remote::ConfiguredRefspec], name: &[u8]) -> Option<RefName> {
    use crate::remote::ConfiguredRefspecKind;
    let matches = |pattern: &[u8]| -> Option<Vec<u8>> {
        match crate::bytes::split_once(pattern, b'*') {
            None => (pattern == name).then(Vec::new),
            Some((prefix, suffix)) => name
                .strip_prefix(prefix)?
                .strip_suffix(suffix)
                .map(<[u8]>::to_vec),
        }
    };
    let excluded = refspecs.iter().any(|spec| {
        spec.kind() == ConfiguredRefspecKind::Exclusion && spec.source().and_then(matches).is_some()
    });
    if excluded {
        return None;
    }
    refspecs
        .iter()
        .filter(|spec| spec.kind() == ConfiguredRefspecKind::Mapping)
        .find_map(|spec| {
            let captured = matches(spec.source()?)?;
            let destination = spec.destination()?;
            let destination = match crate::bytes::split_once(destination, b'*') {
                None => destination.to_vec(),
                Some((prefix, suffix)) => [prefix, &captured, suffix].concat(),
            };
            RefName::new(destination).ok()
        })
}

/// Records the advertised values of the pushed references.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
fn record_targets(
    advertisement: &crate::push::PushAdvertisement,
    names: &[RefName],
    advertised: &std::cell::RefCell<BTreeMap<RefName, Option<ObjectId>>>,
) {
    let mut advertised = advertised.borrow_mut();
    for name in names {
        advertised.insert(name.clone(), advertisement.target(name));
    }
}

/// Like Git, treats a rejected reference whose remote value already equals the requested value as
/// up to date.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
fn mark_up_to_date(report: &mut PushReport, advertised: &BTreeMap<RefName, Option<ObjectId>>) {
    for status in &mut report.refs {
        let Some(current) = advertised.get(&status.command.name) else {
            continue;
        };
        // Deletions are never up to date; see `git push --force-with-lease`.
        let up_to_date = !status.command.deletes() && *current == Some(status.command.new);
        if up_to_date && status.status != Some(crate::push::Status::Ok) {
            status.status = Some(crate::push::Status::Ok);
            status.rejection_origin = None;
        }
    }
}

fn is_knowledge_changed<T>(result: &Result<T, crate::push::PushError>) -> bool {
    matches!(
        result,
        Err(crate::push::PushError::NotSent(
            crate::push::PushFailure::KnowledgeChanged(_)
        ))
    )
}

#[cfg(feature = "http")]
fn is_unauthorized_push<T>(result: &Result<T, crate::push::PushError>) -> bool {
    matches!(
        result,
        Err(crate::push::PushError::NotSent(
            crate::push::PushFailure::Http(crate::transport::http::HttpError::Status(401))
        ))
    )
}

#[cfg(feature = "http")]
fn is_unauthorized_fetch<T>(result: &Result<T, crate::fetch::FetchError>) -> bool {
    matches!(
        result,
        Err(crate::fetch::FetchError::Http(
            crate::transport::http::HttpError::Status(401)
        ))
    )
}

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
fn runtime() -> Result<tokio::runtime::Runtime, TransferError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(TransferError::local)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;
    use crate::push::{ForcePolicy, PushError};

    fn git(directory: &Path, args: &[&str], date: usize) -> String {
        let date = format!("@{} +0000", 1_700_000_000 + date * 60);
        let output = Command::new("git")
            .current_dir(directory)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Push Author")
            .env("GIT_AUTHOR_EMAIL", "push@example.com")
            .env("GIT_COMMITTER_NAME", "Push Author")
            .env("GIT_COMMITTER_EMAIL", "push@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .expect("Git required for push fixtures");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A worktree whose `main` has 30 commits, each adding a file, pushed to a bare `origin`, and
    /// a `topic` branch one commit ahead of `main` that the destination lacks.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, ObjectId) {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        let dest = root.path().join("dest.git");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&dest).unwrap();
        git(&work, &["init", "-q", "--initial-branch=main", "."], 0);
        git(&dest, &["init", "-q", "--bare", "."], 0);
        git(&dest, &["config", "receive.unpackLimit", "1"], 0);
        for n in 0..=30 {
            if n == 30 {
                git(&work, &["push", "-q", &dest.to_string_lossy(), "main"], n);
                git(&work, &["switch", "-q", "-c", "topic"], n);
            }
            std::fs::write(work.join(format!("file-{n}")), format!("{n}\n")).unwrap();
            git(&work, &["add", "."], n);
            git(&work, &["commit", "-q", "-m", &n.to_string()], n);
        }
        git(
            &work,
            &["remote", "add", "origin", &dest.to_string_lossy()],
            0,
        );
        let topic = git(&work, &["rev-parse", "topic"], 0).parse().unwrap();
        (root, work, dest, topic)
    }

    fn push(
        work: &Path,
        topic: ObjectId,
        before_prepare: &dyn Fn(),
    ) -> Result<PushReport, TransferError> {
        Repository::open(work).unwrap().push_observed(
            "origin",
            vec![PushCommand {
                name: RefName::new("refs/heads/topic").unwrap(),
                expected: None,
                new: topic,
                force: ForcePolicy::FastForwardOnly,
            }],
            &PushOptions::default(),
            &Environment::default(),
            &mut NoCallbacks,
            before_prepare,
        )
    }

    /// Pack identities independently listed by Git, including verified thin-pack repair bases.
    fn pack_ids(dest: &Path) -> std::collections::BTreeMap<PathBuf, Vec<String>> {
        std::fs::read_dir(dest.join("objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "idx"))
            .map(|index| {
                let output = Command::new("git")
                    .arg("show-index")
                    .stdin(std::fs::File::open(&index).unwrap())
                    .output()
                    .unwrap();
                assert!(output.status.success());
                let ids = String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
                    .collect();
                (index, ids)
            })
            .collect()
    }

    fn packed(dest: &Path) -> usize {
        pack_ids(dest).values().map(Vec::len).sum()
    }

    #[test]
    fn push_retry_after_knowledge_change_reuses_advertisement() {
        let (_root, work, dest, topic) = fixture();
        let before = pack_ids(&dest);
        let base_tree = git(&work, &["rev-parse", "main~1^{tree}"], 0);
        let attempts = Cell::new(0);
        // The destination's only ref moves back one commit while the first pack is prepared, so
        // the root that pack relies on is no longer advertised.
        let report = push(&work, topic, &|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                git(&dest, &["update-ref", "refs/heads/main", "main~1"], 0);
            }
        })
        .unwrap();
        assert!(report.all_succeeded(), "{report:?}");
        assert_eq!(attempts.get(), 2);
        // The retry sends exactly Git's six selected objects. Native thin-pack repair additionally
        // appends the verified old tree base, rather than retransmitting receiver history.
        let after = pack_ids(&dest);
        let mut incoming: Vec<_> = after
            .iter()
            .filter(|(path, _)| !before.contains_key(*path))
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect();
        let oracle = git(
            &work,
            &["rev-list", "--objects", "topic", "--not", "main~1"],
            0,
        );
        let mut expected: Vec<_> = oracle
            .lines()
            .map(|line| line.split_whitespace().next().unwrap().to_owned())
            .collect();
        assert_eq!(expected.len(), 6);
        expected.push(base_tree);
        incoming.sort();
        expected.sort();
        assert_eq!(incoming, expected);
        assert_eq!(git(&dest, &["rev-parse", "topic"], 0), topic.to_string());
        git(&dest, &["fsck", "--strict", "--no-dangling"], 0);
    }

    #[test]
    fn push_reports_a_second_knowledge_change_without_sending() {
        let (_root, work, dest, topic) = fixture();
        let before = packed(&dest);
        let attempts = Cell::new(0);
        let result = push(&work, topic, &|| {
            attempts.set(attempts.get() + 1);
            git(&dest, &["update-ref", "refs/heads/main", "main~1"], 0);
        });
        assert!(
            matches!(
                result,
                Err(TransferError::Push(ref error))
                    if matches!(**error, PushError::NotSent(PushFailure::KnowledgeChanged(_)))
            ),
            "{result:?}"
        );
        assert_eq!(attempts.get(), 2);
        assert_eq!(packed(&dest), before);
        assert!(git(&dest, &["for-each-ref", "refs/heads/topic"], 0).is_empty());
    }

    #[cfg(feature = "http")]
    #[test]
    fn native_store_accepts_plain_paths_and_leaves_shell_syntax_to_git() {
        let environment = Environment::default()
            .with("HOME", "/home/user")
            .with_current_dir("/work");
        let files = |text| native_store(text, &environment).map(|store| store.files().to_vec());
        assert_eq!(
            files("store --file=~/creds"),
            Some(vec![PathBuf::from("/home/user/creds")])
        );
        assert_eq!(
            files("store --file creds"),
            Some(vec![PathBuf::from("/work/creds")])
        );
        assert_eq!(files("store").map(|files| files.len()), Some(2));
        for text in [
            "store --file=creds>out",
            "store --file creds|tee",
            "store --file=a;b",
            "store --file=$HOME/creds",
            "store --file='creds'",
            "store --file=(creds)",
            "store --file=a&",
            "store --file=<in",
            "store --other",
            "store --file a b",
            "store>out",
        ] {
            assert!(files(text).is_none(), "{text}");
        }
    }

    #[test]
    fn tracking_ref_rejects_overlapping_pattern_halves() {
        use crate::remote::{ConfiguredRefspec, Direction};
        let refspecs =
            [
                ConfiguredRefspec::parse(Direction::Fetch, b"refs/heads/*/x:refs/remotes/o/*")
                    .unwrap(),
            ];
        assert_eq!(tracking_ref(&refspecs, b"refs/heads/x"), None);
        assert_eq!(
            tracking_ref(&refspecs, b"refs/heads/a/x")
                .unwrap()
                .as_bytes(),
            b"refs/remotes/o/a"
        );
    }
}

#[cfg(test)]
#[path = "transfer_fetch_tests.rs"]
mod fetch_tests;

#[cfg(test)]
#[path = "transfer_pack_tests.rs"]
mod pack_tests;
