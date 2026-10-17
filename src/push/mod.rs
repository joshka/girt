//! Send objects to another repository and conditionally update its references.
//!
//! A push has two phases:
//!
//! 1. **Prepare.** [`PreparedPush::new`] takes the commands, verifies the history they publish,
//!    proves each branch update fast-forwards unless force is allowed, and writes the outgoing pack
//!    to a temporary file. Neither repository changes, and nothing is sent.
//! 2. **Send.** A transport sends the commands and the pack. The receiver checks each command's
//!    expected old value and applies or rejects it independently, then reports per reference.
//!
//! [`crate::workflow::push`] does both for a remote named in configuration, preparing after it has
//! read the receiver's advertisement. Use this module when the application chooses the endpoint,
//! limits or receiver history itself.
//!
//! # Commands and force
//!
//! A [`PushCommand`] says "if `name` is `expected`, make it `new`": `expected: None` creates a
//! reference that must not exist, and the null ID deletes one. There is no unconditional push.
//! [`ForcePolicy::FastForwardOnly`] (the default) refuses at preparation to move a branch to a
//! commit that doesn't descend from its old value, or to replace a tag; [`ForcePolicy::Allow`]
//! permits it, still conditional on `expected`, like Git's `--force-with-lease`. Get `expected`
//! from what you last saw on the remote, such as a remote-tracking reference.
//!
//! # Receiver history
//!
//! Pass the object IDs the receiver already has as receiver roots (`&[]` for a full transfer).
//! Preparation then sends only history the receiver lacks, and may use the receiver's objects as
//! delta bases, producing a thin pack. Roots usually come from the receiver's advertisement:
//! [`PushAdvertisement::receiver_roots`] after reading it, which is what
//! [`crate::workflow::push`] does, as `git send-pack` does. A root missing from the live
//! advertisement when sending fails the push with [`PushFailure::KnowledgeChanged`] before any
//! command is sent.
//!
//! # Transports
//!
//! - [`send_local`] writes into a local repository directly, without starting Git. It honors the
//!   receiver's `receive.*` configuration and runs its receive hooks on macOS and Linux.
//!   [`LocalPushRequest`] adds inherited configuration and a reflog identity.
//! - [`send`] speaks receive-pack over caller-owned blocking streams.
//! - With the `http` and `ssh` features, async `send_http` and `send_ssh` run on the caller's Tokio
//!   runtime. `send_http_checked` and `send_ssh_checked_with_progress` let the caller inspect the
//!   live advertisement and decline the whole batch before anything is sent.
//!
//! Wire push requires the `report-status` capability; deletion requires `delete-refs`, and push
//! options require `push-options`. `report-status-v2` is used when advertised. Atomic and signed
//! pushes are unsupported. Both SHA-1 and SHA-256 are supported.
//!
//! # Results and errors
//!
//! A push that reached the receiver returns a [`PushReport`], even when references were rejected:
//! check [`PushReport::all_succeeded`] or each [`RefStatus`]. A [`PushError`] says how far a failed
//! push got:
//!
//! - [`PushError::NotSent`]: no reference updates were attempted on the receiver. Fix the cause and
//!   retry. A native local push may already have stored objects or run hooks.
//! - [`PushError::Uncertain`]: sending began, so some updates may have been applied. Its partial
//!   report holds the acknowledgements received; read the receiver's references before retrying.
//!
//! [`PushFailure`] names the cause in both cases. Optional sideband progress is kept as bounded,
//! untrusted bytes in [`PushReport::progress`].
//!
//! # Concurrency
//!
//! For [`send_local`], the caller must exclude garbage collection, checkout, changes to `HEAD` or
//! checked-out branches, and worktree registration changes in the destination from preparation
//! through publication; reference locks only protect the references themselves. Preparation,
//! hashing and compression are synchronous CPU and disk work: from async code, prepare on a
//! blocking thread.
//!
//! # Example: pushing to a local repository
//!
//! Push a commit (setup hidden) to create `main` in an empty local destination. `expected: None`
//! requires the destination reference to be absent:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::refs::Target;
//! use girt::{InitKind, ObjectFormat, PackLimits, Repository};
//! use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send_local};
//! # use girt::refs::{Expected, RefName};
//! # use girt::{Commit, CommitFields, Signature, Tree};
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
//!
//! let destination =
//!     Repository::init(ObjectFormat::Sha1, directory.path().join("remote.git"), InitKind::Bare)?;
//! let cancel = AtomicBool::new(false);
//! let objects = source.objects(PackLimits::default())?;
//! let command = PushCommand::new(main.clone(), None, commit).with_force(ForcePolicy::FastForwardOnly);
//! let prepared = PreparedPush::new(
//!     &objects, vec![command], &[], PushLimits::default(), &cancel,
//! )?;
//! assert_eq!(prepared.object_count(), 2); // The commit and its empty tree.
//!
//! let report = send_local(
//!     destination.git_dir(),
//!     girt::push::LocalPushRequest::new(&prepared),
//!     girt::transport::TransportControl::new(&cancel),
//!     |_| {}
//! )?;
//! assert!(report.all_succeeded(), "individual references can be rejected: {report:?}");
//! assert_eq!(destination.references()?.read(&main)?, Some(Target::Direct(commit)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub(crate) use ssh::send_ssh_preparing;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use ssh::{
    SshPushOutcome, send_ssh, send_ssh_checked_with_progress, send_ssh_selected_with_progress,
};

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod live_progress;

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub(crate) use http::send_http_preparing;
#[cfg(feature = "http")]
pub use http::{HttpPushOutcome, send_http, send_http_checked, send_http_checked_with_progress};

mod graph;
mod hooks;
mod local;
mod local_native;
mod prepared;
mod progress;
mod protocol;
mod quarantine;
mod types;

pub use local::{LocalPushRequest, send_local};
pub(crate) use local::{ReceiverPolicy, send_local_preparing_observed};
pub use prepared::PreparedPush;
pub use progress::PreparationProgress;
#[cfg(any(fuzzing, test))]
pub(crate) use protocol::fuzz_exchange;
pub use protocol::{PushAdvertisement, send};
pub use types::{
    ForcePolicy, PushCommand, PushError, PushFailure, PushLimits, PushReport, RefRewrite,
    RefStatus, RejectionOrigin, Status,
};

#[cfg(test)]
mod tests;
