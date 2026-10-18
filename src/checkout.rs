//! Conservative tree checkout with literal blob bytes and native POSIX modes.
//!
//! [`Repository::checkout_tree`](crate::Repository::checkout_tree) takes an explicit clean
//! baseline and target, with default [`Limits`] and no cancellation;
//! [`Repository::checkout_tree_with`](crate::Repository::checkout_tree_with) also takes limits and
//! a cancellation flag. [`Report`] records completed operations and
//! index publication; [`Failure`] preserves that report and cleanup failures when a later phase
//! stops. Inspect the actual worktree before recovery because mutations are not rolled back.
//!
//! [`Repository::checkout_tree_configured`](crate::Repository::checkout_tree_configured) uses
//! `core.symlinks` from the repository configuration: false writes symlink target bytes as regular
//! files while preserving symlink entries in the index. Baseline content is still verified,
//! including assume-unchanged entries. Raw checkout always writes native symlinks.
//!
//! The selected tree replaces a clean baseline and index without changing HEAD or references.
//! Pass the tree represented by the current index as the baseline, or `None` for an empty index
//! (including a no-checkout clone). A nonempty baseline with a missing index is rejected. An unborn
//! HEAD needs no special treatment because HEAD is neither read nor switched. Staged changes
//! relative to the supplied baseline, conflicts and dirty tracked paths are refused, even on
//! unchanged paths.
//!
//! Callers must exclude other worktree writers and renames of the root/ancestors throughout the
//! call, and protect repository metadata, objects and mount topology from replacement. The index
//! lock excludes cooperating index writers. Descriptor-relative no-follow traversal, exact names,
//! content verification and repeated identity checks detect observed changes, but POSIX has no
//! conditional rename/unlink by inode: arbitrary writers racing the final check are not excluded.
//! Hardlink/mount attacks and ABA replacements are outside this contract. No batch atomicity,
//! automatic rollback, crash durability, branch switching or force mode is provided;
//! [`Repository::reset`](crate::Repository::reset) with
//! [`ResetMode::Hard`](crate::reset::ResetMode::Hard) uses a variant that discards local changes.
//!
//! Linux/macOS only, on filesystems with native hard links, symlinks and POSIX modes. Colons,
//! backslashes and non-ASCII names are ordinary name bytes.
//!
//! Names follow the working tree's volume. Case folds when `core.ignoreCase` is true or the
//! working tree's root resolves `.GIT` to its `.git` entry; Unicode normalization folds on macOS,
//! the only platform where Git reads `core.precomposeUnicode`. Target paths sharing a folded name
//! are resolved as Git resolves them and listed in [`Report::collisions`]; see [`Collision`]. A
//! file and a directory sharing a name, a spelling change between baseline and target of one name
//! (such as a case-only rename), and any existing entry the volume merges with a wanted name are
//! refused before mutation.
//!
//! Both platforms reject unsafe components and the
//! metadata names Git refuses: `.git` in any ASCII case, the NTFS spellings of `.git` (`git~1`,
//! trailing dots or spaces, `:stream`) unless `core.protectNTFS` is false, and on macOS the HFS+
//! spellings with ignorable code points unless `core.protectHFS` is false. Also refused are
//! general gitlink transitions, nested repositories and index extensions other than `TREE`, `FSMN`,
//! `UNTR`, `EOIE` and `IEOT`. Intent-to-add and skip-worktree
//! entries require caller policy and are refused. An absent baseline with an empty index can
//! initialize gitlinks: their external commit IDs stay in the index and their worktree directories
//! are created empty, without reading objects or entering child repositories. Existing nonempty
//! child directories and name collisions involving gitlinks are refused. Path components are
//! limited to 255 bytes. Windows returns unsupported. Raw checkout preserves blob bytes without
//! attributes, filters, EOL conversion, ignores or config overrides;
//! [`Repository::checkout_tree_with_filters`](crate::Repository::checkout_tree_with_filters)
//! writes regular files through a [`filter::Pipeline`](crate::filter::Pipeline) instead. Symlink
//! blobs must be nonempty, NUL-free and at most 1024 bytes. Mode changes replace files, with
//! permissions 0644/0755; ownership, ACLs and other metadata are not preserved.
//!
//! Preparation also projects the nested-repository marker policy across the planned operations,
//! including retained siblings and directories. A non-root directory containing `HEAD`, `objects`
//! and `refs` (including ASCII case aliases) is refused even when these are ordinary files. This
//! deliberately excludes some valid Git trees so checkout cannot trigger its own live guard after
//! starting to mutate the worktree. The live guards remain active at every mutation boundary.
//!
//! Preparation holds the index lock, verifies trees/blobs, clean tracked content, names and
//! obstructions, then constructs the replacement index before worktree mutation. Mutation removes
//! changed tracked leaves, removes only known empty directories needed for directory-to-file
//! transitions, creates missing directories and installs leaves. Each operation rechecks its path
//! and ancestors. Publication rechecks all target content and the index before renaming the index
//! lock. Preparation and publication enumerate each directory's names once; mutation instead
//! creates every directory and leaf exclusively, so a name that appeared after preparation,
//! including a filesystem case or normalization alias, is refused rather than replaced. Leaves are
//! written under their final names, as Git writes them, so a concurrent reader can observe a file
//! whose content is still being written. An install is reported only once its content is
//! complete; a failed write removes the partial leaf while it is still the file checkout created.
//! Derived `TREE`, `FSMN`, `UNTR`, `EOIE` and `IEOT` caches are discarded, including on
//! unchanged checkouts. Git can rebuild them; girt does not consult fsmonitor or untracked caches.
//! New index entries have zero cached stat words and cleared assume-valid flags; Git may
//! need `git update-index --refresh` before stat-only plumbing such as `git diff-files`.
//! Unrelated content and empty directories are retained. Failures report completed operations
//! in execution order, the active stage and owned artifacts whose cleanup failed. Earlier
//! operations remain applied; inspect the report and actual files before recovery, rather than
//! blindly retrying.
//!
//! [`crate::Repository::checkout_merge`] uses the same guarded execution with a prepared marker
//! tree and original conflict stages. It validates the separate staged replacement before
//! working-file mutation and publishes it under the held index guard. See [`crate::merge`].
//!
//! # Example
//!
//! Materialize a tree into a new working tree whose index is still absent, as after a no-checkout
//! clone. HEAD and references are unchanged:
//!
//! ```
//! use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
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
//! let loose = repository.loose_objects();
//! let id = loose.write_blob(b"#!/bin/sh\necho hello\n")?;
//! let entry = TreeEntry {
//!     mode: EntryMode::Executable,
//!     name: b"hello.sh".to_vec(),
//!     id,
//! };
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?;
//!
//! // `None` baseline: the current index is empty or absent.
//! repository.checkout_tree(None, Some(tree))?;
//!
//! let file = repository
//!     .worktree()
//!     .expect("ordinary repository")
//!     .join("hello.sh");
//! assert_eq!(std::fs::read(&file)?, b"#!/bin/sh\necho hello\n");
//!
//! #[cfg(unix)]
//! {
//!     use std::os::unix::fs::PermissionsExt;
//!     assert_ne!(std::fs::metadata(&file)?.permissions().mode() & 0o111, 0);
//! }
//!
//! let index = repository
//!     .read_index(Default::default())?
//!     .expect("checkout published an index");
//! assert_eq!(index.entries()[0].path, b"hello.sh");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
mod types;
mod workflow;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod worktree;

pub use types::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use worktree::{LOCAL_CHANGES, Selection, run_discarding, run_with_index};
