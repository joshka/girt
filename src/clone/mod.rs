//! Clone into a new bare repository or an ordinary repository without checkout.
//!
//! A clone is a fetch into a repository that doesn't exist yet, in three steps:
//!
//! 1. [`CloneRequest::prepare_tracking`] chooses the destination path, the layout
//!    ([`InitKind`](crate::InitKind)), the origin URL to record, which branch becomes local
//!    ([`BranchSelection`]), and the reflog policy. It checks that the destination doesn't exist,
//!    and creates nothing.
//! 2. A receive adapter reads the source's advertisement, plans the clone, transfers and validates.
//!    [`CloneRequest::receive_local`] returns a [`CloneReady`]; with the `http` or `ssh` feature,
//!    `receive_http` and `receive_ssh` return a [`CloneDownload`] to validate on a blocking worker,
//!    as the fetch adapters do. Nothing is written yet.
//! 3. [`CloneReady::finish`] creates the repository, installs the pack, publishes
//!    `refs/remotes/origin/*` and tags, writes the `origin` remote and branch configuration, and
//!    sets the local branch and `HEAD`. It returns a [`CloneReport`].
//!
//! # How the result differs from `git clone`
//!
//! Every remote branch lands in `refs/remotes/origin/*`, with at most one local branch, in both
//! layouts: a bare clone does not copy every branch into `refs/heads/*` as `git clone --bare`
//! does. No index or working files are written, which is Git's `--no-checkout` state; until the
//! caller runs [`Repository::checkout_tree`](crate::Repository::checkout_tree), `git status`
//! shows the tracked files as deleted. Use [`ClonePlan`] (from [`CloneRequest::plan`]) to preview
//! the branches and `HEAD` a clone would produce.
//!
//! # Failures
//!
//! [`CloneFailure`] from preparation and [`CloneTransferError`] from receiving mean nothing was
//! created. [`CloneError`] from [`CloneReady::finish`] carries a [`CloneReport`] saying which
//! phases completed. girt never rolls back or deletes the destination: the caller decides whether
//! to remove it or keep it for inspection. A fetch cannot resume a failed clone: it neither repairs
//! partial initialization nor writes the origin configuration, local branch and `HEAD` that
//! `finish` publishes last. Removing the destination and cloning again is the simple recovery.
//! See
//! `examples/clone_repository.rs` for a disposable local workflow.
//!
//! # Example: cloning a local repository
//!
//! Clone a local source repository (setup hidden) into a new ordinary repository without checkout:
//!
//! ```
//! use std::ops::ControlFlow;
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::clone::{BranchSelection, CloneRequest};
//! use girt::fetch::{FetchLimits, FetchUpdateLimits};
//! use girt::InitKind;
//! use girt::refs::{Reflog, Target};
//! use girt::transport::TransportControl;
//! # use girt::refs::{Expected, RefName};
//! # use girt::{Commit, CommitFields, ObjectFormat, Repository, Signature, Tree};
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
//! let source_path = source.git_dir().canonicalize()?;
//! let url = source_path.to_str().expect("temporary paths are UTF-8 here");
//! let destination = directory.path().join("clone");
//! let request = CloneRequest::prepare_tracking(
//!     &destination,
//!     InitKind::Worktree,
//!     url.as_bytes(),
//!     BranchSelection::Default,
//!     Reflog::Preserve,
//! )?;
//! let cancel = AtomicBool::new(false);
//! let ready = request.receive_local(
//!     &source_path,
//!     FetchLimits::default(),
//!     TransportControl::new(&cancel),
//!     |_progress| ControlFlow::Continue(()),
//! )?;
//! assert!(!destination.exists(), "transfer alone creates nothing");
//!
//! let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
//! let clone = report.repository.as_ref().expect("finish created the repository");
//! assert_eq!(clone.references()?.read(&main)?, Some(Target::Direct(commit)));
//! assert!(!clone.git_dir().join("index").exists(), "no checkout was performed");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod config;
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod network;
mod plan;
mod workflow;

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
pub use network::CloneDownload;
pub use plan::{BranchSelection, CloneHead, ClonePlan, ClonePlanError};
pub use workflow::{
    CloneError, CloneFailure, CloneReady, CloneReport, CloneRequest, CloneTransferError,
};
