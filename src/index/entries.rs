use crate::ObjectId;

/// One candidate tree leaf, with a byte path and cached stat information.
///
/// Fields are editable drafts. [`super::Index::new`] and [`super::Index::replace_entries`] check
/// paths and relationships before accepting them. Paths must have nonempty `/`-separated
/// components other than `.`, `..`, or `.git`, and contain no NUL. No UTF-8, case folding,
/// platform aliases, backslashes, drive letters or checkout-safety checks are applied.
/// IDs are preserved even when zero; object existence and type remain caller obligations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Full repository-relative path bytes, without a trailing slash.
    pub path: Vec<u8>,
    /// Canonical leaf mode (directories are unsupported).
    pub mode: Mode,
    /// Identity in the index's selected object format; not resolved by index operations.
    pub id: ObjectId,
    /// Normal entry or side of an unresolved conflict.
    pub stage: Stage,
    /// Git's assume-valid bit; consumers must explicitly choose how to honor it.
    pub assume_valid: bool,
    /// Git's intent-to-add bit; the entry is a placeholder until content is staged.
    pub intent_to_add: bool,
    /// Git's skip-worktree bit; this codec does not apply sparse-checkout policy.
    pub skip_worktree: bool,
    /// Cached, uninterpreted 32-bit stat words; not proof the worktree is unchanged.
    pub stat: Stat,
}

impl Entry {
    /// Creates a stage-zero draft with zero stat words and assume-valid cleared.
    ///
    /// Does not validate the path; validation occurs when constructing/replacing index entries.
    pub fn new(path: Vec<u8>, mode: Mode, id: ObjectId) -> Self {
        Self {
            path,
            mode,
            id,
            stage: Stage::Normal,
            assume_valid: false,
            intent_to_add: false,
            skip_worktree: false,
            stat: Stat::default(),
        }
    }
}

/// Canonical index leaf modes. A gitlink names a submodule commit; other modes name blobs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Mode {
    /// Non-executable regular file, `100644`.
    Regular = 0o100644,
    /// Executable regular file, `100755`.
    Executable = 0o100755,
    /// Symbolic link payload, `120000`; reading does not create a symlink.
    Symlink = 0o120000,
    /// Submodule commit, `160000`; reading does not open the submodule.
    Gitlink = 0o160000,
}

/// An entry's position in a conflict, ordered by its on-disk stage number.
///
/// A path has either one normal entry or any nonempty subset of stages 1, 2 and 3. Missing
/// sides represent absent files. Duplicate stages and mixing normal/conflict entries are invalid.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u16)]
pub enum Stage {
    /// Resolved entry, stage 0.
    #[default]
    Normal = 0,
    /// Common ancestor, stage 1.
    Base = 1,
    /// Our side, stage 2.
    Ours = 2,
    /// Their side, stage 3.
    Theirs = 3,
}

/// Raw seconds and nanosecond words retained exactly from the index.
///
/// These are unsigned on-disk words, not host timestamps. No range validation, clock conversion,
/// precision inference or comparison is performed, including for the nanosecond word.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Timestamp {
    /// Raw seconds word.
    pub seconds: u32,
    /// Raw nanosecond-fraction word.
    pub nanoseconds: u32,
}

/// Cached filesystem metadata, retaining every on-disk word exactly.
///
/// Zero is useful for constructed entries that need later refresh. Callers must account for Git's
/// racy-stat rules before trusting cached data; this API does not detect worktree modifications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stat {
    /// Metadata-change time.
    pub ctime: Timestamp,
    /// Content-change time.
    pub mtime: Timestamp,
    /// Device word.
    pub device: u32,
    /// Inode word.
    pub inode: u32,
    /// User ID word.
    pub uid: u32,
    /// Group ID word.
    pub gid: u32,
    /// File size modulo 2^32.
    pub size: u32,
}
