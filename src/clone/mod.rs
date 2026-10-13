//! Clone into a new bare repository or an ordinary repository without checkout.
//!
//! Start with [`CloneRequest::prepare_tracking`] to choose the destination, layout, origin URL,
//! branch policy, and reflog policy. Transfer from an explicit endpoint, then call
//! [`CloneReady::finish`] to create the repository and publish its initial refs. [`ClonePlan`]
//! describes the selected advertisement; [`CloneReport`] and [`CloneError`] identify completed
//! phases when publication is incomplete.
//!
//! Preparation selects all branches and tags from the actual transfer advertisement. Both layouts
//! use `refs/remotes/origin/*`; bare clone does not copy every branch into `refs/heads/*`.
//! No index or worktree files are created. An ordinary clone therefore has an absent index,
//! and Git status can show tracked files as deleted until the caller performs a checkout.
//!
//! Transfer and validation have no destination effects. Finish reports completed phases and
//! preserves residual state on failure; it never rolls back or deletes the destination. See
//! `examples/clone_repository.rs` for a disposable local workflow. HTTP/SSH return owned downloads
//! for validation on caller-managed bounded workers, exactly as the fetch adapters do.
//!
//! # Example
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
