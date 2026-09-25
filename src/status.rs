//! Read-only staged changes and literal working-tree observations.
//!
//! [`Repository::raw_status`](crate::Repository::raw_status) deliberately compares raw bytes and
//! POSIX executable/symlink modes. It does not apply attributes, filters, EOL conversion, ignore
//! rules, `core.filemode`, `core.symlinks`, or ambient configuration. This is useful for consumers
//! that own normalization policy, and is **not Git-default status**. Results repeat that contract.
//! Cached index stat words and assume-valid never bypass content verification. No index refresh,
//! locks, hooks, writes, rename detection, staging or checkout occur.
//!
//! Traversal is supported on Linux and macOS. Linux preserves non-UTF-8 filename bytes; macOS
//! accepts ASCII paths only because Unicode normalization aliases are not implemented. Exact
//! directory-name matching prevents case-folded aliases from being mistaken for tracked names.
//! Unsafe components and metadata aliases are rejected before conversion to platform paths.
//! NUL, backslashes, colons, empty/dot/parent components and case-insensitive `.git` components
//! are rejected on both platforms. Gitlinks and nested repositories are never recursively
//! inspected.
mod types;
mod workflow;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod worktree;

pub use types::*;
