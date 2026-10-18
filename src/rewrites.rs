//! Inferred file renames and copies between Git trees.
//!
//! Call [`Objects::detect_rewrites`](crate::Objects::detect_rewrites) with [`Options`] to select
//! candidate comparison policy and [`Limits`] to bound work. The returned [`Rewrite`] values carry
//! inferred kind and score; [`Copies`] controls copy inference. A match describes stored content,
//! not recorded history or a working-tree operation.
//! [`Objects::detect_rewrites_or_exact`](crate::Objects::detect_rewrites_or_exact) keeps exact and
//! unique-basename matches when [`Options::candidate_limit`] skips exhaustive edited-content
//! detection, and returns a [`Detection`] that reports the skip.
//!
//! Inference is a heuristic, not recorded history: callers retain merge and copy-history policy.
//! Paths and payloads remain bytes. Attributes, worktree conversion and external diff/filter
//! commands are not consulted.
//!
//! # Example
//!
//! A file moved with a small edit is inferred as a rename from the deleted path:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::rewrites::{Kind, Limits, Options};
//! use girt::{EntryMode, InitKind, ObjectFormat, PackLimits, Repository, Tree, TreeEntry};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Bare,
//! )?;
//!
//! let loose = repo.loose_objects();
//! let tree = |name: &[u8], content: &[u8]| -> Result<_, Box<dyn std::error::Error>> {
//!     let id = loose.write_blob(content)?;
//!     let entry = TreeEntry {
//!         mode: EntryMode::Blob,
//!         name: name.to_vec(),
//!         id,
//!     };
//!     Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?)
//! };
//!
//! let lines: Vec<u8> = (0..20)
//!     .flat_map(|n| format!("line {n}\n").into_bytes())
//!     .collect();
//! let old = tree(b"old.txt", &lines)?;
//! let new = tree(b"new.txt", &[&lines[..], b"one more line\n"].concat())?;
//!
//! let objects = repo.objects_with(PackLimits::default())?;
//! let rewrites = objects.detect_rewrites(
//!     Some(old),
//!     Some(new),
//!     Options::default(),
//!     Limits::default(),
//!     None, // Report rewrites for every destination path.
//!     &AtomicBool::new(false),
//! )?;
//!
//! assert_eq!(rewrites.len(), 1);
//! assert_eq!(
//!     (rewrites[0].source.as_slice(), rewrites[0].target.as_slice()),
//!     (&b"old.txt"[..], &b"new.txt"[..])
//! );
//! assert_eq!(rewrites[0].kind, Kind::Rename);
//! assert!(rewrites[0].similarity >= Options::default().similarity);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
mod detect;
mod similarity;
mod types;

pub use types::{Copies, Detection, Error, InexactSkipped, Kind, Limits, Options, Rewrite};
