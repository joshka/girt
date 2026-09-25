//! Conservative tree checkout with literal blob bytes and native POSIX modes.
//!
//! [`Repository::checkout_tree`](crate::Repository::checkout_tree) replaces a clean baseline and
//! index with a selected tree, without changing HEAD or references. The baseline is explicit:
//! pass the tree represented by the current index, or `None` for an empty index, including a
//! no-checkout clone. A nonempty baseline with a missing index is rejected. An unborn HEAD needs
//! no special treatment because HEAD is neither read nor switched. Staged changes relative to
//! the supplied baseline, conflicts and dirty tracked paths are refused, even on unchanged paths.
//!
//! Callers must exclude other worktree writers and renames of the root/ancestors throughout the
//! call, and protect repository metadata, objects and mount topology from replacement. The index
//! lock excludes cooperating index writers. Descriptor-relative no-follow traversal, exact names,
//! content verification and repeated identity checks detect observed changes, but POSIX has no
//! conditional rename/unlink by inode: arbitrary writers racing the final check are not excluded.
//! Hardlink/mount attacks and ABA replacements are outside this contract. No batch atomicity,
//! automatic rollback, crash durability, branch switching or force mode is provided.
//!
//! Linux/macOS only, on filesystems with native hard links, symlinks and POSIX modes. macOS paths
//! and names in enumerated directories must be ASCII. Both platforms conservatively reject ASCII
//! case aliases, metadata names, unsafe components, gitlinks, nested repositories and non-`TREE`
//! index extensions. Intent-to-add and skip-worktree entries require caller policy and are refused.
//! Path components are limited to 255 bytes. Windows returns unsupported. Raw
//! checkout preserves blob bytes without attributes, filters, EOL conversion, ignores or config
//! overrides. Symlink blobs must be nonempty, NUL-free and at most 1024 bytes. Mode changes replace
//! files, with permissions 0644/0755; ownership, ACLs and other metadata are not preserved.
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
//! lock. Unverified `TREE` caches are discarded, including on unchanged checkouts. New index
//! entries have zero cached stat words and cleared assume-valid flags; Git may
//! need `git update-index --refresh` before stat-only plumbing such as `git diff-files`.
//! Unrelated content and empty directories are retained. Failures report completed operations
//! in execution order, the active stage and owned artifacts whose cleanup failed. Earlier
//! operations remain applied; inspect the report and actual files before recovery, rather than
//! blindly retrying.
mod types;
mod workflow;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod worktree;

pub use types::*;
