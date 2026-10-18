//! Inspect a stopped patch-mail application before deciding how to resume it.
//!
//! Git stores `am` progress in the worktree-private `rebase-apply` directory. Take a bounded
//! [`OperationState`] snapshot, then use [`State::from_operation`] to interpret its mail range.
//! Artifacts borrow the snapshot unchanged. Author scripts are untrusted shell text: this module
//! never executes or decodes them. Inspection does not apply patches, install objects, change the
//! index or working files, publish references, or authorize cleanup.
//!
//! ```no_run
//! use girt::am::State;
//! use girt::{OperationLimits, Repository};
//!
//! let repo = Repository::open("repository")?;
//! let snapshot = repo.operation_state(OperationLimits::default())?;
//! if let Some(state) = State::from_operation(&snapshot)? {
//!     println!("next mail: {} of {}", state.next, state.last);
//!     if let Some(patch) = state.patch {
//!         println!("extracted patch: {} bytes", patch.len());
//!     }
//! }
//! # Ok::<(), girt::Error>(())
//! ```
//!
//! Snapshot limits bound all retained files and directories before interpretation. Callers must
//! exclude concurrent operation writers while taking the snapshot; an interpreted state is an
//! observation, not a lock or a validated resume plan. Apply-backend rebase metadata is refused.
use std::path::Path;

use crate::OperationState;

/// Borrowed progress and artifacts from an `am` operation snapshot.
///
/// Optional extracted files can be absent after interruption before mail preparation. Their
/// presence does not establish that they agree with the original mail, index or HEAD. Unknown
/// options and auxiliary files remain available through the original [`OperationState`].
#[derive(Debug)]
#[non_exhaustive]
pub struct State<'a> {
    /// One-based next mail number; `last + 1` represents a completed range awaiting cleanup.
    pub next: usize,
    /// Final mail number, always positive.
    pub last: usize,
    /// Original numbered message, absent only when the range is complete.
    pub mail: Option<&'a [u8]>,
    /// Extracted patch bytes, if Git prepared them.
    pub patch: Option<&'a [u8]>,
    /// Proposed commit-message bytes, if Git prepared them.
    pub message: Option<&'a [u8]>,
    /// Opaque author-script bytes; never execute untrusted snapshot content.
    pub author_script: Option<&'a [u8]>,
}

/// Operation metadata cannot be interpreted as an `am` mail range.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A `rebase-apply` root exists without the `applying` marker, or has a `rebasing` marker.
    #[error("rebase-apply metadata is not an am operation")]
    NotAm,
    /// A required counter, range or current mail is missing or malformed.
    #[error("malformed am state: {0}")]
    Malformed(&'static str),
}

impl<'a> State<'a> {
    /// Interprets one already-bounded operation snapshot without additional filesystem reads.
    ///
    /// Returns `None` when `rebase-apply` is absent. Numbered messages use Git's minimum four-digit
    /// padding. Only the active mail is required; earlier/future mail and extracted artifacts are
    /// not validated. No script, option file or author identity is interpreted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAm`] for non-`am` metadata and [`Error::Malformed`] for missing/invalid
    /// counters, an impossible range, or missing active mail. Work is linear in counter bytes;
    /// returned artifacts borrow the snapshot and allocate no additional body buffers.
    pub fn from_operation(snapshot: &'a OperationState) -> Result<Option<Self>, Error> {
        if !snapshot.roots().any(|root| root == "rebase-apply") {
            return Ok(None);
        }
        let file = |name: &str| snapshot.file(&Path::new("rebase-apply").join(name));
        if file("applying").is_none() || file("rebasing").is_some() {
            return Err(Error::NotAm);
        }
        let next = counter(file("next"), "next")?;
        let last = counter(file("last"), "last")?;
        if next == 0 || last == 0 || next - 1 > last {
            return Err(Error::Malformed("mail range"));
        }
        let mail = if next <= last {
            Some(file(&format!("{next:04}")).ok_or(Error::Malformed("current mail"))?)
        } else {
            None
        };
        Ok(Some(Self {
            next,
            last,
            mail,
            patch: file("patch"),
            message: file("final-commit"),
            author_script: file("author-script"),
        }))
    }
}

fn counter(bytes: Option<&[u8]>, name: &'static str) -> Result<usize, Error> {
    let bytes = bytes.ok_or(Error::Malformed(name))?;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if bytes.is_empty() {
        return Err(Error::Malformed(name));
    }
    bytes.iter().try_fold(0usize, |value, byte| {
        if !byte.is_ascii_digit() {
            return Err(Error::Malformed(name));
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(usize::from(byte - b'0')))
            .ok_or(Error::Malformed(name))
    })
}
