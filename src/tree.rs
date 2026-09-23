use std::cmp::Ordering;
use std::collections::HashSet;

use crate::ObjectId;

/// An owned, in-memory SHA-1 Git tree: names, modes, and references to other objects.
///
/// A tree payload can be readable even when its entries violate tree rules. For example, two
/// records can both name `hello.txt`, or their names can be out of Git order. Inspection tools need
/// to read those records to report the problem, and an object's identity depends on its exact
/// encoded bytes, including entry order. Sorting or removing entries would create a different
/// object; rejecting them during parsing would prevent inspection through this API.
///
/// The operations therefore make separate promises:
///
/// - [`Tree::parse`] decodes supported records and preserves their bytes. Success means the modes,
///   delimiters, and 20-byte IDs can be read; it does not establish valid names or ordering.
/// - [`Tree::validate`] checks component names, uniqueness, and Git order without changing entries.
///   Use it after parsing when your operation requires those properties. Inspection, hashing, and
///   exact re-encoding do not require it.
/// - [`Tree::new`] constructs a tree from caller-supplied entries. It rejects invalid or duplicate
///   names and sorts entries into Git order. Its result already passes validation.
///
/// A parsed tree remains available when validation fails, so callers can inspect its entries and
/// decide what to report or repair. A caller choosing to reconstruct it with [`Tree::new`] must
/// expect a changed identity if sorting changes the payload; invalid or duplicate names still need
/// an explicit correction. Validation returns a result, not a new type or a repaired tree.
///
/// Parsing supports only the five exact mode spellings in [`EntryMode`]; historical permissions
/// and zero-padded modes are rejected. Every accepted payload encodes byte-for-byte unchanged.
///
/// Names are bytes, without UTF-8 conversion. No operation resolves object references or checks
/// their existence or type. This API does not traverse directories, access storage, validate
/// checkout safety on a particular filesystem, or support SHA-256 trees. Memory use is proportional
/// to the supplied payload; callers must bound input size when reading untrusted objects.
///
/// ```
/// use girt::{EntryMode, ObjectId, Tree, TreeEntry};
///
/// let tree = Tree::new(vec![TreeEntry {
///     mode: EntryMode::Blob,
///     name: b"hello.txt".to_vec(),
///     id: ObjectId::for_blob(b"hello\n"),
/// }])?;
///
/// let payload = tree.encode();
/// let parsed = Tree::parse(&payload)?;
///
/// // Require valid names and ordering when accepting a payload from another source.
/// // This check is redundant for the unchanged encoding of Tree::new above.
/// parsed.validate()?;
///
/// assert_eq!(parsed.entries()[0].name, b"hello.txt");
/// assert_eq!(parsed.id(), tree.id());
/// # Ok::<(), girt::TreeError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// Consumes entries, validates them, and sorts them into Git's tree order.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::InvalidName`] for empty names, NUL, `/`, `.` or `..`, and
    /// [`TreeError::DuplicateName`] for repeated byte-identical names, regardless of mode.
    /// Names such as `.git` and platform-specific aliases are not checked: this is structural
    /// validation, not a complete `git fsck` or checkout-safety check. Object IDs are not validated
    /// against a database, and even all-zero IDs are preserved.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self, TreeError> {
        validate_names(&entries)?;
        entries.sort_by(TreeEntry::git_cmp);
        Ok(Self { entries })
    }

    /// Parses a tree payload, excluding the `tree <length>\0` object header.
    ///
    /// Copies names and IDs into owned entries. Does not sort or validate names or duplicates;
    /// [`Tree::validate`] provides that separate check. This allows inspection of readable but
    /// invalid trees, such as those containing duplicate names. Every accepted payload
    /// round-trips exactly.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing mode/name delimiter, unsupported or malformed mode spelling,
    /// or fewer than 20 bytes for an entry's SHA-1 ID. Input must be a SHA-1 payload; no
    /// hash-format autodetection is possible from these bytes.
    pub fn parse(mut payload: &[u8]) -> Result<Self, TreeError> {
        let mut entries = Vec::new();

        while !payload.is_empty() {
            let space = payload
                .iter()
                .position(|&byte| byte == b' ')
                .ok_or(TreeError::MissingModeDelimiter)?;
            let mode = EntryMode::parse(&payload[..space])?;
            payload = &payload[space + 1..];

            let nul = payload
                .iter()
                .position(|&byte| byte == 0)
                .ok_or(TreeError::MissingNameDelimiter)?;
            let name = &payload[..nul];
            payload = &payload[nul + 1..];

            let raw_id = payload.get(..20).ok_or(TreeError::TruncatedObjectId)?;
            let mut id = [0; 20];
            id.copy_from_slice(raw_id);

            entries.push(TreeEntry {
                mode,
                name: name.to_vec(),
                id: ObjectId::from_bytes(id),
            });
            payload = &payload[20..];
        }

        Ok(Self { entries })
    }

    /// Borrows entries in their stored order, without allowing mutation of the tree.
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Checks structural names, unique names, and Git order without changing the tree.
    ///
    /// Call this after [`Tree::parse`] when accepting a tree for operations that rely on these
    /// properties. It is unnecessary for inspection or exact re-encoding, and redundant for a tree
    /// returned by [`Tree::new`]. Returns `Ok(())` on success; on failure, the tree remains
    /// available for inspection. Does not repair entries or check object references or checkout
    /// safety.
    ///
    /// # Errors
    ///
    /// Returns the name errors documented by [`Tree::new`], or [`TreeError::Unsorted`] if entries
    /// are out of Git order. The same object-reference and checkout-safety exclusions apply.
    pub fn validate(&self) -> Result<(), TreeError> {
        validate_names(&self.entries)?;

        if self
            .entries
            .windows(2)
            .any(|pair| pair[0].git_cmp(&pair[1]).is_gt())
        {
            return Err(TreeError::Unsorted);
        }

        Ok(())
    }

    /// Encodes the payload as repeated `mode SP name NUL raw-id` records.
    ///
    /// Returns an owned buffer without an object header or compression. Preserves parsed order and
    /// names even when [`Tree::validate`] would fail; never silently repairs an existing object.
    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();

        for entry in &self.entries {
            payload.extend_from_slice(entry.mode.bytes());
            payload.push(b' ');
            payload.extend_from_slice(&entry.name);
            payload.push(0);
            payload.extend_from_slice(entry.id.as_bytes());
        }

        payload
    }

    /// Hashes `tree <decimal payload length>\0` followed by the exact encoded payload.
    ///
    /// Allocates a temporary payload buffer. Does not validate or normalize a parsed tree.
    pub fn id(&self) -> ObjectId {
        ObjectId::for_object("tree", &self.encode())
    }
}

/// One tree entry, with an uninterpreted byte name and a SHA-1 reference.
///
/// Fields are freely editable before construction. [`Tree::new`] checks names and duplicates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEntry {
    /// The entry's Git type and executable status.
    pub mode: EntryMode,

    /// A single path component as bytes, without text conversion.
    pub name: Vec<u8>,

    /// Referenced blob, tree, or commit identity; existence and type are not checked.
    pub id: ObjectId,
}

impl TreeEntry {
    /// Compares names using Git's byte order, treating a tree's name as ending in `/`.
    ///
    /// Other modes, including gitlinks, use a NUL terminator. IDs do not affect order. For example,
    /// file `a.c` precedes tree `a`, which precedes file `a0`. This does not validate names; the
    /// Git ordering contract applies to valid single-component names.
    pub fn git_cmp(&self, other: &Self) -> Ordering {
        let left = self
            .name
            .iter()
            .copied()
            .chain(std::iter::once(self.mode.terminator()));
        let right = other
            .name
            .iter()
            .copied()
            .chain(std::iter::once(other.mode.terminator()));

        left.cmp(right)
    }
}

/// Supported modes with their exact octal spelling in an encoded tree payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryMode {
    /// Regular, non-executable blob (`100644`).
    Blob,
    /// Executable blob (`100755`).
    Executable,
    /// Symbolic link whose target bytes are stored in a blob (`120000`).
    Symlink,
    /// Subtree (`40000`, without a leading zero).
    Tree,
    /// Submodule commit reference (`160000`); ordered like a file, not a subtree.
    Gitlink,
}

impl EntryMode {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Blob => b"100644",
            Self::Executable => b"100755",
            Self::Symlink => b"120000",
            Self::Tree => b"40000",
            Self::Gitlink => b"160000",
        }
    }

    fn parse(bytes: &[u8]) -> Result<Self, TreeError> {
        match bytes {
            b"100644" => Ok(Self::Blob),
            b"100755" => Ok(Self::Executable),
            b"120000" => Ok(Self::Symlink),
            b"40000" => Ok(Self::Tree),
            b"160000" => Ok(Self::Gitlink),
            _ => Err(TreeError::UnsupportedMode),
        }
    }

    fn terminator(self) -> u8 {
        if self == Self::Tree { b'/' } else { 0 }
    }
}

/// A tree payload could not be parsed, or entries failed structural validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TreeError {
    /// No space separates the mode from the name.
    #[error("missing tree mode delimiter")]
    MissingModeDelimiter,
    /// The mode is not one of the five supported exact octal spellings.
    #[error("unsupported or malformed tree entry mode")]
    UnsupportedMode,
    /// No NUL separates the name from the raw identity.
    #[error("missing tree name delimiter")]
    MissingNameDelimiter,
    /// Fewer than 20 raw identity bytes follow the name delimiter.
    #[error("truncated tree entry object identity")]
    TruncatedObjectId,
    /// A name is empty, contains NUL or `/`, or is `.` or `..`.
    #[error("invalid tree entry name")]
    InvalidName,
    /// Two entries have byte-identical names, even if their modes differ.
    #[error("duplicate tree entry name")]
    DuplicateName,
    /// Entries are not in Git's tree order.
    #[error("tree entries are not in Git order")]
    Unsorted,
}

fn validate_names(entries: &[TreeEntry]) -> Result<(), TreeError> {
    let mut names = HashSet::with_capacity(entries.len());

    for entry in entries {
        let name = entry.name.as_slice();
        if name.is_empty()
            || name.contains(&0)
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
        {
            return Err(TreeError::InvalidName);
        }

        // Duplicate file/tree names need not be adjacent in Git order (a, a.c, a/).
        if !names.insert(name) {
            return Err(TreeError::DuplicateName);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn entry(mode: EntryMode, name: &[u8]) -> TreeEntry {
        TreeEntry {
            mode,
            name: name.to_vec(),
            id: ObjectId::from_bytes([0x81; 20]),
        }
    }

    // Literal framing plus a fixed identity keeps expected bytes independent of the encoder.
    fn record(prefix: &[u8]) -> Vec<u8> {
        [prefix, &[0x81; 20]].concat()
    }

    #[test]
    fn empty_tree_has_git_identity() {
        let tree = Tree::new(vec![]).unwrap();

        assert_eq!(tree.encode(), b"");
        assert_eq!(Tree::parse(b"").unwrap(), tree);
        assert_eq!(tree.validate(), Ok(()));
        assert_eq!(
            tree.id().to_string(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
    }

    #[rstest]
    #[case::regular(EntryMode::Blob, b"100644 a\0")]
    #[case::executable(EntryMode::Executable, b"100755 a\0")]
    #[case::symlink(EntryMode::Symlink, b"120000 a\0")]
    #[case::directory(EntryMode::Tree, b"40000 a\0")]
    #[case::gitlink(EntryMode::Gitlink, b"160000 a\0")]
    fn preserves_supported_modes(#[case] mode: EntryMode, #[case] prefix: &[u8]) {
        let expected = record(prefix);
        let tree = Tree::new(vec![entry(mode, b"a")]).unwrap();

        assert_eq!(tree.encode(), expected);
        assert_eq!(
            Tree::parse(&expected).unwrap().entries(),
            &[entry(mode, b"a")]
        );
    }

    #[rstest]
    #[case::non_utf8(b"\xff\x80")]
    #[case::whitespace(b"a b\t\n")]
    #[case::backslash(b"a\\b")]
    fn preserves_name_bytes(#[case] name: &[u8]) {
        let expected = [b"100644 ".as_slice(), name, b"\0", &[0x81; 20]].concat();
        let tree = Tree::new(vec![entry(EntryMode::Blob, name)]).unwrap();

        assert_eq!(tree.encode(), expected);
        assert_eq!(Tree::parse(&expected).unwrap().entries()[0].name, name);
    }

    #[rstest]
    #[case::file_prefix(EntryMode::Blob, b"a", EntryMode::Blob, b"a.c")]
    #[case::directory_after_dot(EntryMode::Blob, b"a.c", EntryMode::Tree, b"a")]
    #[case::directory_before_zero(EntryMode::Tree, b"a", EntryMode::Blob, b"a0")]
    #[case::longer_directory(EntryMode::Tree, b"a", EntryMode::Tree, b"aa")]
    #[case::gitlink_is_file(EntryMode::Gitlink, b"a", EntryMode::Blob, b"a.c")]
    #[case::symlink_is_file(EntryMode::Symlink, b"a", EntryMode::Tree, b"a")]
    #[case::unsigned_bytes(EntryMode::Blob, b"z", EntryMode::Blob, b"\x80")]
    fn compares_git_names(
        #[case] left_mode: EntryMode,
        #[case] left_name: &[u8],
        #[case] right_mode: EntryMode,
        #[case] right_name: &[u8],
    ) {
        let left = entry(left_mode, left_name);
        let right = entry(right_mode, right_name);

        assert_eq!(left.git_cmp(&right), Ordering::Less);
        assert_eq!(right.git_cmp(&left), Ordering::Greater);
    }

    #[test]
    fn sorts_file_and_directory_prefixes() {
        let tree = Tree::new(vec![
            entry(EntryMode::Blob, b"a0"),
            entry(EntryMode::Tree, b"a"),
            entry(EntryMode::Blob, b"a.c"),
        ])
        .unwrap();
        let names: Vec<_> = tree
            .entries()
            .iter()
            .map(|entry| entry.name.as_slice())
            .collect();

        assert_eq!(names, [b"a.c".as_slice(), b"a", b"a0"]);
        assert_eq!(tree.validate(), Ok(()));
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::nul(b"a\0b")]
    #[case::slash(b"a/b")]
    #[case::dot(b".")]
    #[case::dot_dot(b"..")]
    fn rejects_invalid_new_names(#[case] name: &[u8]) {
        assert_eq!(
            Tree::new(vec![entry(EntryMode::Blob, name)]),
            Err(TreeError::InvalidName)
        );
    }

    #[rstest]
    #[case::empty(b"100644 \0")]
    #[case::slash(b"100644 a/b\0")]
    #[case::dot(b"100644 .\0")]
    #[case::dot_dot(b"100644 ..\0")]
    fn parses_invalid_names_without_repair(#[case] prefix: &[u8]) {
        let payload = record(prefix);
        let tree = Tree::parse(&payload).unwrap();

        assert_eq!(tree.encode(), payload);
        assert_eq!(tree.validate(), Err(TreeError::InvalidName));
    }

    #[test]
    fn preserves_unsorted_input_until_explicit_reconstruction() {
        let payload = [record(b"40000 a\0"), record(b"100644 a.c\0")].concat();
        let tree = Tree::parse(&payload).unwrap();

        assert_eq!(tree.encode(), payload);
        assert_eq!(tree.validate(), Err(TreeError::Unsorted));

        let rebuilt = Tree::new(tree.entries().to_vec()).unwrap();

        assert_eq!(rebuilt.entries()[0].name, b"a.c");
        assert_ne!(rebuilt.id(), tree.id());
    }

    #[rstest]
    #[case::same_mode(b"100644 a\0")]
    #[case::different_mode(b"40000 a\0")]
    fn detects_duplicate_names_even_when_nonadjacent(#[case] last: &[u8]) {
        let payload = [record(b"100644 a\0"), record(b"100644 a.c\0"), record(last)].concat();
        let tree = Tree::parse(&payload).unwrap();

        assert_eq!(tree.encode(), payload);
        assert_eq!(tree.validate(), Err(TreeError::DuplicateName));
        assert_eq!(
            Tree::new(tree.entries().to_vec()),
            Err(TreeError::DuplicateName)
        );
    }

    #[rstest]
    #[case::empty(b" a\0")]
    #[case::non_octal(b"100648 a\0")]
    #[case::negative(b"-100644 a\0")]
    #[case::historical_permissions(b"100664 a\0")]
    #[case::padded_tree(b"040000 a\0")]
    #[case::unknown_type(b"140000 a\0")]
    #[case::overflow(b"777777777777777777777777 a\0")]
    fn rejects_unsupported_mode_spellings(#[case] prefix: &[u8]) {
        assert_eq!(
            Tree::parse(&record(prefix)),
            Err(TreeError::UnsupportedMode)
        );
    }

    #[rstest]
    #[case::missing_space(b"100644", TreeError::MissingModeDelimiter)]
    #[case::missing_nul(b"100644 name", TreeError::MissingNameDelimiter)]
    #[case::no_id(b"100644 a\0", TreeError::TruncatedObjectId)]
    #[case::short_id(b"100644 a\0abcdefghijklmnopqrs", TreeError::TruncatedObjectId)]
    fn rejects_incomplete_records(#[case] payload: &[u8], #[case] error: TreeError) {
        assert_eq!(Tree::parse(payload), Err(error));
    }

    #[test]
    fn rejects_trailing_partial_entry() {
        let payload = [record(b"100644 a\0"), b"100644 b\0short".to_vec()].concat();

        assert_eq!(Tree::parse(&payload), Err(TreeError::TruncatedObjectId));
    }
}
