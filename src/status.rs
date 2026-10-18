//! Read-only staged changes and configured or raw working-tree observations.
//!
//! Call [`Repository::raw_status`](crate::Repository::raw_status) with [`Baseline`] to choose the
//! comparison tree and [`Untracked`] to choose whether raw untracked leaves are listed. It uses
//! default [`Limits`] and never cancels;
//! [`Repository::raw_status_with`](crate::Repository::raw_status_with) takes limits and a
//! cancellation flag. [`Report`]
//! separates staged, unstaged, unmerged, unchecked gitlink, and boundary observations. Empty
//! change lists do not establish Git-default cleanliness or authorize checkout.
//!
//! [`Repository::raw_status`](crate::Repository::raw_status) deliberately compares raw bytes and
//! POSIX executable/symlink modes. It does not apply attributes, filters, EOL conversion, ignore
//! rules, `core.filemode`, `core.symlinks`, or ambient configuration. This is useful for consumers
//! that own normalization policy, and is **not Git-default status**.
//!
//! [`Repository::status_with_filters`](crate::Repository::status_with_filters) converts each
//! working file it reads through a [`filter::Pipeline`](crate::filter::Pipeline) before hashing,
//! as `git status` does for line-ending conversion, `ident` and filter drivers such as Git LFS.
//!
//! [`Repository::status`](crate::Repository::status) and filtered status apply intent-to-add,
//! assume-unchanged and skip-worktree flags, and ignore regular-file executable-bit changes when
//! `core.fileMode=false`. Status without filters compares literal content bytes. Both forms
//! list untracked paths as `git status` does: pass the rules from
//! [`Repository::ignore_rules`](crate::Repository::ignore_rules) and an [`UntrackedFiles`] listing,
//! and read [`Report::untracked`].
//! As in Git, a working file is not read when its `lstat` matches every cached index stat word and
//! the entry is not racily clean; index blobs are never read. Configured status also trusts
//! assume-unchanged and skip-worktree promises; raw status inspects those files. No index
//! refresh, locks, hooks, writes, rename detection, staging or checkout occur.
//!
//! Traversal is supported on Linux and macOS. Linux preserves non-UTF-8 filename bytes; macOS
//! accepts ASCII paths only because Unicode normalization aliases are not implemented. Exact
//! directory-name matching prevents case-folded aliases from being mistaken for tracked names.
//! Unsafe components and metadata aliases are rejected before conversion to platform paths.
//! NUL, backslashes, colons, empty/dot/parent components and case-insensitive `.git` components
//! are rejected on both platforms. Gitlinks and nested repositories are never recursively
//! inspected.
//!
//! # Example
//!
//! Stage a file in the index, change it in the working tree, and observe a raw unstaged change. The
//! report is an observation; nothing is refreshed or written:
//!
//! ```
//! use girt::index::{Entry, Mode};
//! use girt::status::{Baseline, Change, Untracked};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! # if cfg!(not(any(target_os = "linux", target_os = "macos"))) {
//! #     return Ok(());
//! # }
//! let directory = tempfile::tempdir()?;
//! let repository = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("w"),
//!     InitKind::Worktree,
//! )?;
//!
//! let id = repository.loose_objects().write_blob(b"staged\n")?;
//! let mut edit = repository.edit_index(Default::default())?;
//! edit.replace_entries(vec![Entry::new(b"hello.txt".to_vec(), Mode::Regular, id)])?;
//! edit.commit()?;
//!
//! let worktree = repository.worktree().expect("ordinary repository");
//! std::fs::write(worktree.join("hello.txt"), b"working\n")?;
//! std::fs::write(worktree.join("scratch.txt"), b"new\n")?;
//!
//! let report = repository.raw_status(Baseline::Head, Untracked::RawFilesWithoutIgnores)?;
//! // HEAD is unborn, so the whole index is staged relative to an empty tree.
//! assert_eq!(report.staged[0].path, b"hello.txt");
//! assert_eq!(report.unstaged[0].path, b"hello.txt");
//! assert!(matches!(report.unstaged[0].change, Change::Modified(_)));
//! assert_eq!(report.raw_untracked, [b"scratch.txt".to_vec()]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
mod types;
mod workflow;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod worktree;

pub use types::*;
