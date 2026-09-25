use sha1::{Digest, Sha1};
use thiserror::Error;

use super::{Entry, Mode, Stage, Stat, Timestamp};
use crate::ObjectId;

/// Owned, structurally valid SHA-1 v2 index with immutable entry access.
///
/// Parsing verifies the checksum, canonical modes, flags, paths, padding, ordering and stage
/// relationships. It does not verify object targets, cached stat data or opaque extension payloads.
/// Accepted bytes round-trip exactly: encoding regenerates canonical name lengths and zero padding,
/// which parsing already requires, and preserves extension order and payloads. Construction sorts
/// entries; parsing never repairs their order. Editing may discard a `TREE` cache explicitly under
/// the policy in [`Self::replace_entries`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Index {
    entries: Vec<Entry>,
    extensions: Vec<Extension>,
}

/// Opaque optional extension preserved in its original position and byte representation.
///
/// Payload semantics are not validated or used. Mandatory extensions cannot enter an [`Index`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extension {
    signature: [u8; 4],
    data: Vec<u8>,
}
impl Extension {
    /// Four-byte signature whose first byte is ASCII uppercase.
    pub fn signature(&self) -> [u8; 4] {
        self.signature
    }
    /// Original payload bytes; their internal format has not been checked.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Resource ceilings applied independently to each parse, construction, edit or encode operation.
///
/// Input/output bytes include the header, extensions and checksum. Entry and extension counts
/// bound owned record overhead; path bytes are additionally bounded per entry. Allocations are
/// proportional to these ceilings, not a precise resident-memory budget. Caller-owned drafts and
/// input buffers are outside the budget. Storage can retain original, parsed and encoded buffers
/// simultaneously and reads at most `max_bytes + 1` bytes to detect an oversized file.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum complete encoded bytes; default 256 MiB.
    pub max_bytes: usize,
    /// Maximum entries; default one million.
    pub max_entries: usize,
    /// Maximum bytes in one path; default one MiB.
    pub max_path_bytes: usize,
    /// Maximum optional extensions; default 64.
    pub max_extensions: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_path_bytes: 1024 * 1024,
            max_extensions: 64,
        }
    }
}

/// Index format, unsupported feature or resource-limit failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum Error {
    /// A supplied identity is not SHA-1; this operation does not yet support SHA-256.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Header, entry or extension framing is malformed at a byte offset.
    #[error("malformed index at byte {offset}: {reason}")]
    Malformed {
        /// Offset in the complete file.
        offset: usize,
        /// Structural failure.
        reason: &'static str,
    },
    /// Stored SHA-1 does not match the bytes preceding it (zero checksums are not accepted).
    #[error("index checksum mismatch")]
    Checksum,
    /// Only v2 is supported, including explicit rejection of recognized v3/v4.
    #[error("unsupported index version {0}")]
    Version(u32),
    /// An entry is invalid or outside the supported baseline.
    #[error("invalid index entry {entry}: {reason}")]
    Entry {
        /// Zero-based entry position, after sorting for construction/editing.
        entry: usize,
        /// Path, mode, flags or relationship failure.
        reason: &'static str,
    },
    /// A mandatory extension is not supported.
    #[error("unsupported mandatory index extension {0:?}")]
    MandatoryExtension([u8; 4]),
    /// An edit could invalidate opaque information that cannot safely be discarded.
    #[error("index extension {0:?} prevents editing")]
    ExtensionPreventsEdit([u8; 4]),
    /// A configured ceiling or representable format length was exceeded.
    #[error("index resource limit exceeded: {0}")]
    Limit(&'static str),
}

impl Index {
    /// Validates and sorts drafts by unsigned path bytes, then stage.
    ///
    /// Creates an index without extensions. Prefix-related paths are allowed across different
    /// conflict stages (directory/file conflicts), but forbidden within the same stage or when
    /// either entry is normal. No staging policy, conflict resolution or object reads are
    /// performed.
    ///
    /// # Errors
    ///
    /// Rejects invalid paths, duplicates, normal/conflict mixtures, file/directory collisions in
    /// the same stage, or exhausted limits. Caller-owned input is consumed even on failure.
    pub fn new(mut entries: Vec<Entry>, limits: Limits) -> Result<Self, Error> {
        check_count(entries.len(), limits.max_entries, "entries")?;
        entries.sort_unstable_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        validate_entries(&entries, limits)?;
        let index = Self {
            entries,
            extensions: Vec::new(),
        };
        index.encoded_len(limits)?;
        Ok(index)
    }

    /// Borrows entries in validated byte-path/stage order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    /// Borrows opaque optional extensions in original order.
    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    /// Replaces the complete entry set after validation and sorting, without staging policy.
    ///
    /// Clone the current entries to form drafts, then submit the complete desired set. A changed
    /// set invalidates and removes all `TREE` extensions. Any other extension, including `REUC`
    /// and unknown optional signatures, refuses the edit rather than lose semantic information
    /// or retain a stale cache. An identical set preserves every extension unchanged.
    ///
    /// # Errors
    ///
    /// Returns construction/limit errors or [`Error::ExtensionPreventsEdit`]. On every failure
    /// the original index, including its extensions, remains unchanged.
    pub fn replace_entries(&mut self, entries: Vec<Entry>, limits: Limits) -> Result<(), Error> {
        let replacement = Self::new(entries, limits)?;
        if replacement.entries == self.entries {
            self.encoded_len(limits)?;
            return Ok(());
        }
        if let Some(extension) = self.extensions.iter().find(|e| e.signature != *b"TREE") {
            return Err(Error::ExtensionPreventsEdit(extension.signature));
        }
        *self = replacement;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn discard_tree_cache(&mut self) {
        self.extensions
            .retain(|extension| extension.signature != *b"TREE");
    }

    /// Parses a complete index, copying paths and optional extensions into owned storage.
    ///
    /// SHA-1 input must be selected by the caller; the header does not identify the hash format.
    /// Checksum validation precedes entry allocation. All supported structural invariants hold on
    /// success; optional extension internals remain opaque. No filesystem paths are materialized.
    ///
    /// # Errors
    ///
    /// Rejects malformed/truncated input, bad checksums, unsupported versions/modes/extended
    /// flags, mandatory extensions, invalid paths/order/stages, and exhausted limits.
    pub fn parse(bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        check_count(bytes.len(), limits.max_bytes, "bytes")?;
        if bytes.len() < 32 || &bytes[..4] != b"DIRC" {
            return Err(malformed(0, "missing header or checksum"));
        }
        let version = word(bytes, 4);
        if version != 2 {
            return Err(Error::Version(version));
        }
        let end = bytes.len() - 20;
        if Sha1::digest(&bytes[..end])[..] != bytes[end..] {
            return Err(Error::Checksum);
        }
        let count = word(bytes, 8) as usize;
        check_count(count, limits.max_entries, "entries")?;
        if count > (end - 12) / 64 {
            return Err(malformed(8, "entry count exceeds available bytes"));
        }
        let mut cursor = 12;
        let mut entries = Vec::with_capacity(count);
        for position in 0..count {
            entries.push(parse_entry(&bytes[..end], &mut cursor, position, limits)?);
        }
        let mut extensions = Vec::new();
        while cursor < end {
            check_count(extensions.len() + 1, limits.max_extensions, "extensions")?;
            if end - cursor < 8 {
                return Err(malformed(cursor, "truncated extension header"));
            }
            let signature: [u8; 4] = bytes[cursor..cursor + 4].try_into().unwrap();
            if !signature[0].is_ascii_uppercase() {
                return Err(Error::MandatoryExtension(signature));
            }
            let length = word(bytes, cursor + 4) as usize;
            cursor += 8;
            if length > end - cursor {
                return Err(malformed(cursor, "truncated extension payload"));
            }
            extensions.push(Extension {
                signature,
                data: bytes[cursor..cursor + length].to_vec(),
            });
            cursor += length;
        }
        validate_entries(&entries, limits)?;
        Ok(Self {
            entries,
            extensions,
        })
    }

    /// Encodes canonical v2 framing and a SHA-1 checksum, preserving all stored information.
    ///
    /// Parsing already requires canonical framing, so unedited parsed indexes round-trip exactly.
    /// New entries use derived name-length flags and zero padding. Stat words and opaque optional
    /// extensions are unchanged; no worktree or object database is accessed.
    ///
    /// # Errors
    ///
    /// Fails before output allocation when an encoded size/count/path exceeds the supplied limits.
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        let length = self.encoded_len(limits)?;
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"DIRC");
        put_word(&mut out, 2);
        put_word(&mut out, self.entries.len() as u32);
        for entry in &self.entries {
            encode_entry(&mut out, entry);
        }
        for extension in &self.extensions {
            out.extend_from_slice(&extension.signature);
            put_word(&mut out, extension.data.len() as u32);
            out.extend_from_slice(&extension.data);
        }
        let checksum = Sha1::digest(&out);
        out.extend_from_slice(&checksum);
        Ok(out)
    }

    fn encoded_len(&self, limits: Limits) -> Result<usize, Error> {
        check_count(
            self.entries.len(),
            limits.max_entries.min(u32::MAX as usize),
            "entries",
        )?;
        check_count(self.extensions.len(), limits.max_extensions, "extensions")?;
        let mut length = 32usize;
        for entry in &self.entries {
            check_count(entry.path.len(), limits.max_path_bytes, "path bytes")?;
            length = length
                .checked_add(entry_len(entry.path.len())?)
                .ok_or(Error::Limit("bytes"))?;
        }
        for extension in &self.extensions {
            check_count(extension.data.len(), u32::MAX as usize, "extension bytes")?;
            length = length
                .checked_add(8)
                .and_then(|n| n.checked_add(extension.data.len()))
                .ok_or(Error::Limit("bytes"))?;
        }
        check_count(length, limits.max_bytes, "bytes")?;
        Ok(length)
    }
}

fn parse_entry(
    bytes: &[u8],
    cursor: &mut usize,
    position: usize,
    limits: Limits,
) -> Result<Entry, Error> {
    let start = *cursor;
    let fixed = bytes
        .get(start..start.saturating_add(62))
        .ok_or(malformed(start, "truncated entry"))?;
    let mode = match word(fixed, 24) {
        0o100644 => Mode::Regular,
        0o100755 => Mode::Executable,
        0o120000 => Mode::Symlink,
        0o160000 => Mode::Gitlink,
        _ => {
            return Err(entry_error(
                position,
                "unsupported mode (including sparse directories)",
            ));
        }
    };
    let flags = u16::from_be_bytes(fixed[60..62].try_into().unwrap());
    if flags & 0x4000 != 0 {
        return Err(entry_error(position, "extended flags unsupported in v2"));
    }
    let path_start = start + 62;
    let search_end = bytes.len().min(
        path_start
            .saturating_add(limits.max_path_bytes)
            .saturating_add(1),
    );
    let name_len = bytes[path_start..search_end]
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| {
            if search_end < bytes.len() {
                Error::Limit("path bytes")
            } else {
                malformed(path_start, "unterminated path")
            }
        })?;
    if (flags & 0xfff) as usize != name_len.min(0xfff) {
        return Err(entry_error(position, "name length flag mismatch"));
    }
    let length = entry_len(name_len)?;
    let end = start.checked_add(length).ok_or(Error::Limit("bytes"))?;
    let padding = bytes
        .get(path_start + name_len..end)
        .ok_or(malformed(path_start, "truncated padding"))?;
    if padding.iter().any(|b| *b != 0) {
        return Err(malformed(path_start + name_len, "nonzero padding"));
    }
    *cursor = end;
    Ok(Entry {
        path: bytes[path_start..path_start + name_len].to_vec(),
        mode,
        id: ObjectId::Sha1(fixed[40..60].try_into().unwrap()),
        stage: match (flags >> 12) & 3 {
            0 => Stage::Normal,
            1 => Stage::Base,
            2 => Stage::Ours,
            _ => Stage::Theirs,
        },
        assume_valid: flags & 0x8000 != 0,
        stat: Stat {
            ctime: Timestamp {
                seconds: word(fixed, 0),
                nanoseconds: word(fixed, 4),
            },
            mtime: Timestamp {
                seconds: word(fixed, 8),
                nanoseconds: word(fixed, 12),
            },
            device: word(fixed, 16),
            inode: word(fixed, 20),
            uid: word(fixed, 28),
            gid: word(fixed, 32),
            size: word(fixed, 36),
        },
    })
}

fn validate_entries(entries: &[Entry], limits: Limits) -> Result<(), Error> {
    for (position, entry) in entries.iter().enumerate() {
        entry.id.require_sha1()?;
        check_count(entry.path.len(), limits.max_path_bytes, "path bytes")?;
        if entry.path.contains(&0)
            || entry
                .path
                .split(|b| *b == b'/')
                .any(|part| matches!(part, b"" | b"." | b".." | b".git"))
        {
            return Err(entry_error(position, "invalid path component or NUL"));
        }
        if position > 0 {
            let previous = &entries[position - 1];
            if (&previous.path, previous.stage) >= (&entry.path, entry.stage) {
                return Err(entry_error(position, "unsorted or duplicate path/stage"));
            }
            if previous.path == entry.path && previous.stage == Stage::Normal {
                return Err(entry_error(
                    position,
                    "normal entry mixed with conflict stages",
                ));
            }
        }
    }
    for (position, entry) in entries.iter().enumerate() {
        // Search each proper path prefix in the sorted entry set. Only up to four records share
        // a name, so conflicts remain bounded without building a second path tree.
        for (slash, _) in entry.path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            let prefix = &entry.path[..slash];
            let start = entries.partition_point(|candidate| candidate.path.as_slice() < prefix);
            for ancestor in entries[start..]
                .iter()
                .take_while(|candidate| candidate.path == prefix)
            {
                if ancestor.stage == entry.stage
                    || ancestor.stage == Stage::Normal
                    || entry.stage == Stage::Normal
                {
                    return Err(entry_error(position, "file/directory collision in a stage"));
                }
            }
        }
    }
    Ok(())
}

fn encode_entry(out: &mut Vec<u8>, entry: &Entry) {
    let start = out.len();
    let stat = entry.stat;
    for value in [
        stat.ctime.seconds,
        stat.ctime.nanoseconds,
        stat.mtime.seconds,
        stat.mtime.nanoseconds,
        stat.device,
        stat.inode,
        entry.mode as u32,
        stat.uid,
        stat.gid,
        stat.size,
    ] {
        put_word(out, value);
    }
    out.extend_from_slice(entry.id.as_bytes());
    let flags = ((entry.assume_valid as u16) << 15)
        | ((entry.stage as u16) << 12)
        | entry.path.len().min(0xfff) as u16;
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&entry.path);
    // encoded_len has already checked this arithmetic.
    out.resize(start + entry_len(entry.path.len()).unwrap(), 0);
}
fn entry_len(path_len: usize) -> Result<usize, Error> {
    path_len
        .checked_add(70)
        .map(|n| n & !7)
        .ok_or(Error::Limit("path bytes"))
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn put_word(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn check_count(actual: usize, maximum: usize, resource: &'static str) -> Result<(), Error> {
    if actual > maximum {
        Err(Error::Limit(resource))
    } else {
        Ok(())
    }
}
fn malformed(offset: usize, reason: &'static str) -> Error {
    Error::Malformed { offset, reason }
}
fn entry_error(entry: usize, reason: &'static str) -> Error {
    Error::Entry { entry, reason }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod format_boundary_tests {
    use super::*;

    #[test]
    fn rejects_sha256_index_entry() {
        let entry = Entry::new(b"a".to_vec(), Mode::Regular, ObjectId::Sha256([1; 32]));
        assert!(matches!(
            Index::new(vec![entry], Limits::default()),
            Err(Error::ObjectFormat(_))
        ));
    }
}
