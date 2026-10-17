//! Convenience operations for caller-trusted repositories and configuration.
//!
//! [`Handle`] owns a repository and lazily opens its object view. References are opened on demand
//! and read live. Reads refresh the object view once on absence, never on corruption or a limit
//! failure. Configuration and shallow boundaries remain snapshots; reopen after changing them.
//! Like [`crate::workflow`], transfers may run configured credential helpers and SSH commands.
//! Limits remain the core defaults; trust does not select unlimited bounds. Shared reads take
//! `&self`; iterators own their reader state. Initialization/refresh publish complete snapshots
//! under a mutex, while revision resolution and content reads run without that lock.
//!
//! Use [`Handle::repository`] for explicit operations, cancellation, custom limits and recovery.
//! Mutations require exclusive ownership of the working tree and repository state across the call.
//! Commit and switch do not run hooks, filters or signing programs. Status loads repository ignore
//! rules and collapses untracked directories. Tracked comparisons and checkout use raw bytes
//! without normalization policy. Clone, patch rendering and blame remain separate.
//!
//! ```
//! use girt::trusted::Handle;
//! use girt::{InitKind, ObjectFormat, Repository};
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Bare,
//! )?;
//! let git = Handle::new(repo);
//! let blob = git.repository().loose_objects().write_blob(b"hello\n")?;
//! assert_eq!(git.rev_parse(blob)?, blob);
//! # Ok::<(), girt::Error>(())
//! ```

mod handle;
mod log;
mod reader;

pub use handle::{Error, Handle};
pub use log::Log;
