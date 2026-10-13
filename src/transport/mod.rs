//! Caller control for owned transports; protocol streams remain caller-owned and cooperative.
//!
//! [`TransportControl`] carries a cancellation flag and optional deadline into native local,
//! HTTP, and SSH adapters. It checks between work steps; it cannot interrupt a synchronous hash,
//! filesystem call, or caller-owned blocking stream. The `http` feature adds owned HTTP transfers;
//! `ssh` adds an OpenSSH adapter on macOS/Linux. Those adapters document their runtime, process,
//! authentication, and cleanup contracts at the endpoint.
//!
//! For a full fetch lifecycle start with [`crate::fetch::FetchRequest`]; for push, prepare
//! [`crate::push::PreparedPush`] before choosing a transport. Applications own worker scheduling
//! and must inspect partial outcomes before retrying publication.
//!
//! # Example
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
//!     send_local_with_control,
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
//! # let command = PushCommand {
//! #     name: main.clone(),
//! #     expected: None,
//! #     new: commit,
//! #     force: ForcePolicy::FastForwardOnly,
//! # };
//! let cancel = AtomicBool::new(false);
//! let prepared = PreparedPush::new(&objects, vec![command], PushLimits::default(), &cancel)?;
//! let control = TransportControl { cancel: &cancel, deadline: Some(Instant::now()) };
//! let result = send_local_with_control(destination.git_dir(), &prepared, control);
//! assert!(matches!(result, Err(PushError::NotSent(PushFailure::Deadline))));
//! assert_eq!(destination.references()?.read(&main)?, None);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub mod ssh;

mod control;
#[cfg(feature = "http")]
pub mod http;
pub use control::TransportControl;
pub(crate) use control::{Interruption, interruption};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod process;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg(test)]
pub(crate) use process::Server;
