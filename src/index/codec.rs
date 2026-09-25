#[cfg(test)]
use sha1::{Digest, Sha1};
use thiserror::Error;

use super::{Entry, Mode, Stage, Stat, Timestamp};
use crate::ObjectId;

/// Owned, structurally valid SHA-1/SHA-256 v2/v3/v4 index with immutable entry access.
///
/// Parsing verifies the checksum, canonical modes, flags, paths, padding, ordering and stage
/// relationships. It does not verify object targets, cached stat data or opaque extension payloads.
/// Accepted bytes round-trip exactly while unchanged, including alternate v4 prefix compression.
/// The original encoding is retained alongside decoded entries. Edits encode canonical name
/// lengths, maximal shared prefixes and zero padding. Construction sorts entries; parsing never
/// repairs their order. Equality compares version, entries and extensions, ignoring alternative
/// byte encodings. Split replacement records resolve before validation; sorted additions are merged
/// with shared entries. Editing applies the extension policy in [`Self::replace_entries`].
#[derive(Clone, Debug)]
pub struct Index {
    pub(super) format: crate::ObjectFormat,
    version: Version,
    // Retain alternate valid compression and flag framing until an actual edit.
    pub(super) original: Option<Vec<u8>>,
    pub(super) entries: Vec<Entry>,
    pub(super) extensions: Vec<Extension>,
}

/// On-disk entry framing. V3 adds extended flags; V4 also compresses adjacent paths.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum Version {
    /// Padded paths without extended flags.
    #[default]
    V2 = 2,
    /// Padded paths with optional intent-to-add and skip-worktree flags.
    V3 = 3,
    /// Prefix-compressed paths without padding, with optional extended flags.
    V4 = 4,
}

// Equality describes index information, not alternative encodings of that information.
impl PartialEq for Index {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format
            && self.version == other.version
            && self.entries == other.entries
            && self.extensions == other.extensions
    }
}
impl Eq for Index {}

/// Opaque optional extension preserved in its original position and byte representation.
///
/// Optional payload semantics are not validated or used. The mandatory `link` extension is
/// validated during shared-index resolution; `sdir` marks validated sparse directory entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extension {
    pub(super) signature: [u8; 4],
    pub(super) data: Vec<u8>,
}
impl Extension {
    /// Four-byte signature; optional extensions begin with ASCII uppercase.
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
/// input buffers are outside the budget. Parsing retains original bytes to preserve framing.
/// Storage can retain original, parsed and encoded buffers
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
    /// A supplied identity differs from the selected index format.
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
    /// Stored checksum does not match the bytes preceding it (zero checksums are not accepted).
    #[error("index checksum mismatch")]
    Checksum,
    /// The version is outside the supported v2/v3/v4 formats.
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
    /// A split index requires the named immutable shared file.
    #[error("split index requires shared index {0}")]
    SharedRequired(ObjectId),
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
    /// Creates an empty index in an explicit repository format, including when no file exists.
    pub fn empty(format: crate::ObjectFormat) -> Self {
        Self {
            format,
            version: Version::V2,
            original: None,
            entries: Vec::new(),
            extensions: Vec::new(),
        }
    }

    /// Returns the format used by every entry and the file checksum.
    pub fn object_format(&self) -> crate::ObjectFormat {
        self.format
    }

    /// Validates and sorts drafts by unsigned path bytes, then stage.
    ///
    /// Creates an index in `format` without extensions, using v3 when extended flags are present,
    /// otherwise v2. Even an empty index retains its format.
    /// Prefix-related paths are allowed across different conflict stages (directory/file
    /// conflicts), but forbidden within the same stage or when either entry is normal. No
    /// staging policy, conflict resolution or object reads are performed.
    ///
    /// # Errors
    ///
    /// Rejects foreign-format identities, invalid paths, duplicates, normal/conflict mixtures,
    /// file/directory collisions in the same stage, or exhausted limits. Caller-owned input is
    /// consumed even on failure.
    pub fn new(
        format: crate::ObjectFormat,
        mut entries: Vec<Entry>,
        limits: Limits,
    ) -> Result<Self, Error> {
        check_count(entries.len(), limits.max_entries, "entries")?;
        entries.sort_unstable_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        validate_entries(format, &entries, limits)?;
        let mut index = Self {
            format,
            version: if entries.iter().any(extended) {
                Version::V3
            } else {
                Version::V2
            },
            original: None,
            entries,
            extensions: Vec::new(),
        };
        index.mark_sparse();
        index.encoded_len(limits)?;
        Ok(index)
    }

    /// Returns the retained or selected entry framing version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Selects output framing, discarding derived caches when the version changes.
    ///
    /// Resolve-undo (`REUC`) is retained. Unknown optional extensions prevent conversion.
    /// Unchanged versions preserve the exact original encoding.
    ///
    /// # Errors
    ///
    /// V2 refuses extended flags. Limit or extension errors leave the index unchanged.
    pub fn set_version(&mut self, version: Version, limits: Limits) -> Result<(), Error> {
        if version == Version::V2 && self.entries.iter().any(extended) {
            return Err(entry_error(0, "extended flags require v3 or v4"));
        }
        if version == self.version {
            self.encoded_len(limits)?;
            return Ok(());
        }
        let mut replacement = self.clone();
        replacement.invalidate_extensions()?;
        replacement.version = version;
        replacement.encoded_len(limits)?;
        *self = replacement;
        Ok(())
    }

    fn invalidate_extensions(&mut self) -> Result<(), Error> {
        if let Some(extension) = self.extensions.iter().find(|e| {
            !matches!(
                &e.signature,
                b"TREE" | b"UNTR" | b"FSMN" | b"EOIE" | b"IEOT" | b"REUC" | b"link" | b"sdir"
            )
        }) {
            return Err(Error::ExtensionPreventsEdit(extension.signature));
        }
        self.extensions.retain(|e| e.signature == *b"REUC");
        self.mark_sparse();
        self.original = None;
        Ok(())
    }

    fn mark_sparse(&mut self) {
        if self.entries.iter().any(|e| e.mode == Mode::SparseDirectory) {
            self.extensions.push(Extension {
                signature: *b"sdir",
                data: Vec::new(),
            });
        }
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
    /// set removes derived `TREE`, `UNTR`, `FSMN`, `EOIE` and `IEOT` caches. `REUC` resolve-undo
    /// records retain their original bytes because they describe prior conflicts independently of
    /// current entries. Unknown optional extensions refuse edits. An identical set preserves every
    /// extension and the original encoding. Changed split indexes deliberately become standalone
    /// full indexes; their immutable shared file is left untouched. The version is retained,
    /// upgrading v2 to v3 when extended flags are introduced.
    ///
    /// # Errors
    ///
    /// Returns construction/limit errors or [`Error::ExtensionPreventsEdit`]. On every failure
    /// the original index, including its extensions, remains unchanged.
    pub fn replace_entries(
        &mut self,
        mut entries: Vec<Entry>,
        limits: Limits,
    ) -> Result<(), Error> {
        check_count(entries.len(), limits.max_entries, "entries")?;
        entries.sort_unstable_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        validate_entries(self.format, &entries, limits)?;
        if entries == self.entries {
            self.encoded_len(limits)?;
            return Ok(());
        }
        let version = if self.version == Version::V2 && entries.iter().any(extended) {
            Version::V3
        } else {
            self.version
        };
        let mut replacement = Self {
            format: self.format,
            version,
            original: None,
            entries,
            extensions: self.extensions.clone(),
        };
        replacement.invalidate_extensions()?;
        replacement.encoded_len(limits)?;
        *self = replacement;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn discard_tree_cache(&mut self) {
        self.original = None;
        self.extensions
            .retain(|extension| !matches!(&extension.signature, b"TREE" | b"link"));
    }

    /// Parses a complete index, copying paths and optional extensions into owned storage.
    ///
    /// The caller must select the repository format; the header does not identify it.
    /// Neither file length nor digest-like bytes select or change that format.
    /// Checksum validation precedes entry allocation. All supported structural invariants hold on
    /// success; optional extension internals remain opaque. No filesystem paths are materialized.
    ///
    /// # Errors
    ///
    /// Rejects malformed/truncated input, bad checksums, unsupported versions/modes or unknown
    /// extended flags, mandatory extensions, invalid paths/order/stages, and exhausted limits.
    pub fn parse(format: crate::ObjectFormat, bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        Self::parse_with_shared(format, bytes, None, limits)
    }

    /// Parses a split index using the supplied shared file, or an ordinary standalone index.
    ///
    /// The checksum named by `link` must match the validated shared file checksum. Limits bound
    /// aggregate encoded input and the resolved entry set. Unchanged encoding preserves the main
    /// file and still requires its shared file; edits deliberately encode a standalone full index.
    /// No filesystem or staging policy is involved.
    ///
    /// # Errors
    ///
    /// Returns structural, dependency, checksum or resource errors. Nested shared indexes fail.
    pub fn parse_with_shared(
        format: crate::ObjectFormat,
        bytes: &[u8],
        shared: Option<&[u8]>,
        limits: Limits,
    ) -> Result<Self, Error> {
        Self::parse_file(format, bytes, limits)?.resolve_shared(shared, limits)
    }

    pub(super) fn parse_file(
        format: crate::ObjectFormat,
        bytes: &[u8],
        limits: Limits,
    ) -> Result<Self, Error> {
        check_count(bytes.len(), limits.max_bytes, "bytes")?;
        if bytes.len() < 12 + format.digest_len() || &bytes[..4] != b"DIRC" {
            return Err(malformed(0, "missing header or checksum"));
        }
        let version = match word(bytes, 4) {
            2 => Version::V2,
            3 => Version::V3,
            4 => Version::V4,
            other => return Err(Error::Version(other)),
        };
        let end = bytes.len() - format.digest_len();
        if *format.checksum(&bytes[..end]).as_bytes() != bytes[end..] {
            return Err(Error::Checksum);
        }
        let count = word(bytes, 8) as usize;
        check_count(count, limits.max_entries, "entries")?;
        if count > (end - 12) / (43 + format.digest_len()) {
            return Err(malformed(8, "entry count exceeds available bytes"));
        }
        let mut cursor = 12;
        let mut entries: Vec<Entry> = Vec::with_capacity(count);
        for position in 0..count {
            entries.push(parse_entry(
                format,
                &bytes[..end],
                &mut cursor,
                position,
                limits,
                version,
                entries.last().map_or(&[], |e| e.path.as_slice()),
            )?);
        }
        let mut extensions = Vec::new();
        while cursor < end {
            check_count(extensions.len() + 1, limits.max_extensions, "extensions")?;
            if end - cursor < 8 {
                return Err(malformed(cursor, "truncated extension header"));
            }
            let signature: [u8; 4] = bytes[cursor..cursor + 4].try_into().unwrap();
            if !signature[0].is_ascii_uppercase() && !matches!(&signature, b"link" | b"sdir") {
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
        let sparse: Vec<_> = extensions
            .iter()
            .filter(|e| e.signature == *b"sdir")
            .collect();
        if sparse.len() > 1 || sparse.first().is_some_and(|e| !e.data.is_empty()) {
            return Err(malformed(cursor, "invalid sparse extension"));
        }
        if entries.iter().any(|e| e.mode == Mode::SparseDirectory) && sparse.is_empty() {
            return Err(malformed(cursor, "sparse directory without sdir extension"));
        }
        if !extensions.iter().any(|e| e.signature == *b"link") {
            validate_entries(format, &entries, limits)?;
        }
        Ok(Self {
            format,
            version,
            original: Some(bytes.to_vec()),
            entries,
            extensions,
        })
    }

    /// Encodes the selected version and hash format, preserving all stored information.
    ///
    /// Unedited parsed indexes return their retained bytes, including nonmaximal v4 compression.
    /// New entries use derived name-length flags and zero padding. Stat words and opaque optional
    /// extensions are unchanged; no worktree or object database is accessed.
    ///
    /// # Errors
    ///
    /// Fails before output allocation when an encoded size/count/path exceeds the supplied limits.
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        let length = self.encoded_len(limits)?;
        if let Some(original) = &self.original {
            return Ok(original.clone());
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"DIRC");
        put_word(&mut out, self.version as u32);
        put_word(&mut out, self.entries.len() as u32);
        let mut previous: &[u8] = &[];
        for entry in &self.entries {
            encode_entry(&mut out, entry, self.version, previous);
            previous = &entry.path;
        }
        for extension in &self.extensions {
            out.extend_from_slice(&extension.signature);
            put_word(&mut out, extension.data.len() as u32);
            out.extend_from_slice(&extension.data);
        }
        let checksum = self.format.checksum(&out);
        out.extend_from_slice(checksum.as_bytes());
        Ok(out)
    }

    pub(super) fn encoded_len(&self, limits: Limits) -> Result<usize, Error> {
        check_count(
            self.entries.len(),
            limits.max_entries.min(u32::MAX as usize),
            "entries",
        )?;
        check_count(self.extensions.len(), limits.max_extensions, "extensions")?;
        let mut length = 12 + self.format.digest_len();
        let mut previous: &[u8] = &[];
        for entry in &self.entries {
            check_count(entry.path.len(), limits.max_path_bytes, "path bytes")?;
            length = length
                .checked_add(encoded_entry_len(entry, self.version, previous)?)
                .ok_or(Error::Limit("bytes"))?;
            previous = &entry.path;
        }
        for extension in &self.extensions {
            check_count(extension.data.len(), u32::MAX as usize, "extension bytes")?;
            length = length
                .checked_add(8)
                .and_then(|n| n.checked_add(extension.data.len()))
                .ok_or(Error::Limit("bytes"))?;
        }
        let length = self.original.as_ref().map_or(length, Vec::len);
        check_count(length, limits.max_bytes, "bytes")?;
        Ok(length)
    }
}

fn parse_entry(
    format: crate::ObjectFormat,
    bytes: &[u8],
    cursor: &mut usize,
    position: usize,
    limits: Limits,
    version: Version,
    previous: &[u8],
) -> Result<Entry, Error> {
    let width = format.digest_len();
    let fixed_len = 42 + width;
    let start = *cursor;
    let fixed = bytes
        .get(start..start.saturating_add(fixed_len))
        .ok_or(malformed(start, "truncated entry"))?;
    let mode = match word(fixed, 24) {
        0o100644 => Mode::Regular,
        0o100755 => Mode::Executable,
        0o120000 => Mode::Symlink,
        0o160000 => Mode::Gitlink,
        0o040000 => Mode::SparseDirectory,
        _ => {
            return Err(entry_error(position, "unsupported mode"));
        }
    };
    let flags = u16::from_be_bytes(fixed[40 + width..fixed_len].try_into().unwrap());
    let mut path_start = start + fixed_len;
    let extended_flags = if flags & 0x4000 != 0 {
        if version == Version::V2 {
            return Err(entry_error(position, "extended flags unsupported in v2"));
        }
        let raw = bytes
            .get(path_start..path_start + 2)
            .ok_or(malformed(path_start, "truncated extended flags"))?;
        path_start += 2;
        let value = u16::from_be_bytes(raw.try_into().unwrap());
        if value & !0x6000 != 0 {
            return Err(entry_error(position, "unknown extended flags"));
        }
        value
    } else {
        0
    };
    let prefix = if version == Version::V4 {
        let remove = read_varint(bytes, &mut path_start)?;
        previous.len().checked_sub(remove).ok_or(malformed(
            path_start,
            "path prefix removes more than previous name",
        ))?
    } else {
        0
    };
    check_count(prefix, limits.max_path_bytes, "path bytes")?;
    let search_end = bytes.len().min(
        path_start
            .saturating_add(limits.max_path_bytes - prefix)
            .saturating_add(1),
    );
    let suffix_len = bytes[path_start..search_end]
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| {
            if search_end < bytes.len() {
                Error::Limit("path bytes")
            } else {
                malformed(path_start, "unterminated path")
            }
        })?;
    let name_len = prefix + suffix_len;
    if (flags & 0xfff) as usize != name_len.min(0xfff) {
        return Err(entry_error(position, "name length flag mismatch"));
    }
    let terminated = path_start + suffix_len + 1;
    let end = if version == Version::V4 {
        terminated
    } else {
        start + ((terminated - start + 7) & !7)
    };
    let padding = bytes
        .get(terminated..end)
        .ok_or(malformed(terminated, "truncated padding"))?;
    if padding.iter().any(|b| *b != 0) {
        return Err(malformed(terminated, "nonzero padding"));
    }
    *cursor = end;
    let mut path = Vec::with_capacity(name_len);
    path.extend_from_slice(&previous[..prefix]);
    path.extend_from_slice(&bytes[path_start..path_start + suffix_len]);
    Ok(Entry {
        path,
        mode,
        id: ObjectId::from_bytes(format, &fixed[40..40 + width]).unwrap(),
        stage: match (flags >> 12) & 3 {
            0 => Stage::Normal,
            1 => Stage::Base,
            2 => Stage::Ours,
            _ => Stage::Theirs,
        },
        assume_valid: flags & 0x8000 != 0,
        intent_to_add: extended_flags & 0x2000 != 0,
        skip_worktree: extended_flags & 0x4000 != 0,
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

pub(super) fn validate_entries(
    format: crate::ObjectFormat,
    entries: &[Entry],
    limits: Limits,
) -> Result<(), Error> {
    for (position, entry) in entries.iter().enumerate() {
        entry.id.require_format(format)?;
        check_count(entry.path.len(), limits.max_path_bytes, "path bytes")?;
        let path = if entry.mode == Mode::SparseDirectory {
            if !entry.skip_worktree || entry.intent_to_add || entry.stage != Stage::Normal {
                return Err(entry_error(
                    position,
                    "invalid sparse directory flags or stage",
                ));
            }
            entry.path.strip_suffix(b"/").ok_or(entry_error(
                position,
                "sparse directory requires trailing slash",
            ))?
        } else {
            &entry.path
        };
        if path.contains(&0)
            || path
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
        if entry.mode == Mode::SparseDirectory
            && entries
                .get(position + 1)
                .is_some_and(|next| next.path.starts_with(&entry.path))
        {
            return Err(entry_error(position, "sparse directory overlaps entries"));
        }
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

fn encode_entry(out: &mut Vec<u8>, entry: &Entry, version: Version, previous: &[u8]) {
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
        | ((extended(entry) as u16) << 14)
        | ((entry.stage as u16) << 12)
        | entry.path.len().min(0xfff) as u16;
    out.extend_from_slice(&flags.to_be_bytes());
    if extended(entry) {
        let flags = ((entry.intent_to_add as u16) << 13) | ((entry.skip_worktree as u16) << 14);
        out.extend_from_slice(&flags.to_be_bytes());
    }
    if version == Version::V4 {
        let common = common_prefix(previous, &entry.path);
        let (prefix, start) = varint(previous.len() - common);
        out.extend_from_slice(&prefix[start..]);
        out.extend_from_slice(&entry.path[common..]);
        out.push(0);
    } else {
        out.extend_from_slice(&entry.path);
        out.resize(
            start + encoded_entry_len(entry, version, previous).unwrap(),
            0,
        );
    }
}
fn extended(entry: &Entry) -> bool {
    entry.intent_to_add || entry.skip_worktree
}
fn common_prefix(left: &[u8], right: &[u8]) -> usize {
    left.iter().zip(right).take_while(|(a, b)| a == b).count()
}
fn encoded_entry_len(entry: &Entry, version: Version, previous: &[u8]) -> Result<usize, Error> {
    let flags_len = if extended(entry) { 2 } else { 0 };
    if version != Version::V4 {
        return entry_len(
            entry.id.format(),
            entry
                .path
                .len()
                .checked_add(flags_len)
                .ok_or(Error::Limit("bytes"))?,
        );
    }
    let common = common_prefix(previous, &entry.path);
    (43 + entry.id.format().digest_len() + flags_len + (10 - varint(previous.len() - common).1))
        .checked_add(entry.path.len() - common)
        .ok_or(Error::Limit("bytes"))
}
// Git's offset encoding increments the accumulated high groups before shifting.
fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<usize, Error> {
    let mut value = 0usize;
    loop {
        let byte = *bytes
            .get(*cursor)
            .ok_or(malformed(*cursor, "truncated path prefix"))?;
        *cursor += 1;
        value = value
            .checked_mul(128)
            .and_then(|n| n.checked_add((byte & 127) as usize))
            .ok_or(malformed(*cursor, "path prefix overflow"))?;
        if byte & 128 == 0 {
            return Ok(value);
        }
        value = value
            .checked_add(1)
            .ok_or(malformed(*cursor, "path prefix overflow"))?;
    }
}
fn varint(mut value: usize) -> ([u8; 10], usize) {
    // Ten seven-bit groups cover every usize on supported 32/64-bit hosts.
    let mut bytes = [0; 10];
    let mut start = 9;
    bytes[start] = (value & 127) as u8;
    while value >= 128 {
        value = (value >> 7) - 1;
        start -= 1;
        bytes[start] = 128 | (value & 127) as u8;
    }
    (bytes, start)
}

fn entry_len(format: crate::ObjectFormat, path_len: usize) -> Result<usize, Error> {
    path_len
        .checked_add(50 + format.digest_len())
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
            Index::new(crate::ObjectFormat::Sha1, vec![entry], Limits::default()),
            Err(Error::ObjectFormat(_))
        ));
    }
}

#[cfg(test)]
mod dual_format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn empty_indexes_require_the_selected_format(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let index = Index::empty(format);
        let bytes = index.encode(Limits::default()).unwrap();
        assert_eq!(bytes.len(), 12 + format.digest_len());
        assert_eq!(
            Index::parse(format, &bytes, Limits::default()).unwrap(),
            index
        );
        assert!(Index::parse(other, &bytes, Limits::default()).is_err());
        let short = Limits {
            max_bytes: bytes.len() - 1,
            ..Limits::default()
        };
        assert!(matches!(index.encode(short), Err(Error::Limit("bytes"))));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn mixed_replacement_preserves_original(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let mut index = Index::new(
            format,
            vec![Entry::new(
                b"a".to_vec(),
                Mode::Regular,
                ObjectId::null(format),
            )],
            Limits::default(),
        )
        .unwrap();
        let before = index.encode(Limits::default()).unwrap();
        let entry = Entry::new(b"b".to_vec(), Mode::Regular, ObjectId::null(other));
        assert!(matches!(
            index.replace_entries(vec![entry], Limits::default()),
            Err(Error::ObjectFormat(_))
        ));
        assert_eq!(index.encode(Limits::default()).unwrap(), before);
        assert_eq!(
            Index::parse(format, &before, Limits::default())
                .unwrap()
                .entries()[0]
                .id,
            ObjectId::null(format)
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn all_truncations_and_corrupt_checksum_are_rejected(#[case] format: ObjectFormat) {
        let id = ObjectId::for_blob(format, b"data");
        let index = Index::new(
            format,
            vec![Entry::new(b"a".to_vec(), Mode::Regular, id)],
            Limits::default(),
        )
        .unwrap();
        let mut bytes = index.encode(Limits::default()).unwrap();
        assert!(
            (0..bytes.len())
                .all(|end| Index::parse(format, &bytes[..end], Limits::default()).is_err())
        );
        bytes[15] ^= 1;
        assert_eq!(
            Index::parse(format, &bytes, Limits::default()),
            Err(Error::Checksum)
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn opaque_extensions_retain_exact_bytes_and_prevent_edits(#[case] format: ObjectFormat) {
        let mut bytes = Index::empty(format).encode(Limits::default()).unwrap();
        bytes.truncate(12);
        bytes.extend_from_slice(b"TEST\0\0\0\x03\xff\0a");
        bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
        let mut index = Index::parse(format, &bytes, Limits::default()).unwrap();
        let replacement = Entry::new(b"a".to_vec(), Mode::Regular, ObjectId::null(format));
        assert_eq!(
            index.replace_entries(vec![replacement], Limits::default()),
            Err(Error::ExtensionPreventsEdit(*b"TEST"))
        );
        assert_eq!(index.encode(Limits::default()).unwrap(), bytes);
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn checksum_does_not_hide_truncated_entry_table(#[case] format: ObjectFormat) {
        let mut bytes = b"DIRC\0\0\0\x02\0\0\0\x01".to_vec();
        bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
        assert!(matches!(
            Index::parse(format, &bytes, Limits::default()),
            Err(Error::Malformed { .. })
        ));
    }
}
