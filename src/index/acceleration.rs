//! Optional status caches decoded from the published format and Git CLI observations.
//!
//! Parsing establishes structure only. Neither cache proves filesystem freshness or authorizes
//! skipping a status observation without checking its environment and querying its monitor.
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted index extension data (#103)"
)]

use super::{Error, Index, Limits, Stat, Timestamp};
use crate::{ObjectFormat, ObjectId};

#[cfg(test)]
mod tests;

/// File-system monitor checkpoint from an `FSMN` index extension.
///
/// Dirty positions refer to the index entries in their stored order. A clear bit is a promise
/// relative to the checkpoint, not proof that a file is still unchanged. Query the same monitor
/// before trusting it. Parsing does not execute a hook or contact a daemon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsmonitorCache {
    token: MonitorToken,
    dirty: Vec<usize>,
    original: Vec<u8>,
}

/// The checkpoint representation selected by the on-disk FSMN version.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MonitorToken {
    /// Protocol-one nanoseconds since the Unix epoch.
    Timestamp(u64),
    /// Protocol-two opaque bytes, without their NUL terminator.
    Opaque(Vec<u8>),
}

impl FsmonitorCache {
    /// Builds a checkpoint for the stored ordering of `entries` index records.
    ///
    /// `dirty` must be sorted, unique and below `entries`. Only record a checkpoint after
    /// observing every entry not marked dirty relative to this token. Construction does not
    /// query a monitor or write the index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for invalid dirty positions or NUL-containing opaque tokens,
    /// and [`Error::Limit`] when the encoded payload or entry count exceeds a ceiling.
    pub fn new(
        token: MonitorToken,
        dirty: Vec<usize>,
        entries: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        if entries > limits.max_entries {
            return Err(Error::Limit("fsmonitor entries"));
        }
        if !dirty.is_sorted_by(|a, b| a < b)
            || dirty.last().is_some_and(|position| *position >= entries)
        {
            return Err(invalid("invalid fsmonitor dirty positions"));
        }
        let count = u32::try_from(entries).map_err(|_| Error::Limit("fsmonitor entries"))?;
        let words = entries.div_ceil(64);
        let bitmap_bytes = words
            .checked_add(1)
            .and_then(|words| words.checked_mul(8))
            .and_then(|bytes| bytes.checked_add(12))
            .ok_or(Error::Limit("bitmap bytes"))?;
        let token_bytes = match &token {
            MonitorToken::Timestamp(_) => 8,
            MonitorToken::Opaque(bytes) => {
                if bytes.contains(&0) {
                    return Err(invalid("NUL in fsmonitor token"));
                }
                bytes
                    .len()
                    .checked_add(1)
                    .ok_or(Error::Limit("fsmonitor token"))?
            }
        };
        let length = bitmap_bytes
            .checked_add(token_bytes)
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(Error::Limit("fsmonitor bytes"))?;
        if length > limits.max_bytes {
            return Err(Error::Limit("fsmonitor bytes"));
        }
        let mut bytes = Vec::with_capacity(length);
        match &token {
            MonitorToken::Timestamp(value) => {
                bytes.extend_from_slice(&1u32.to_be_bytes());
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            MonitorToken::Opaque(value) => {
                bytes.extend_from_slice(&2u32.to_be_bytes());
                bytes.extend_from_slice(value);
                bytes.push(0);
            }
        }
        bytes.extend_from_slice(
            &u32::try_from(bitmap_bytes)
                .map_err(|_| Error::Limit("bitmap bytes"))?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(words + 1)
                .map_err(|_| Error::Limit("bitmap words"))?
                .to_be_bytes(),
        );
        // One zero-length run followed by literal words. The final pointer names that run.
        bytes.extend_from_slice(&((words as u64) << 33).to_be_bytes());
        let mut positions = dirty.iter().copied().peekable();
        for word in 0..words {
            let mut value = 0u64;
            while let Some(position) = positions.next_if(|position| *position / 64 == word) {
                value |= 1u64 << (position % 64);
            }
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&0u32.to_be_bytes());
        Self::parse(&bytes, entries, limits)
    }

    /// Decodes an FSMN payload, bounding bitmap positions by `entries` and `limits.max_entries`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for invalid framing, versions or bitmap positions, and
    /// [`Error::Limit`] when a configured ceiling is exceeded.
    pub fn parse(bytes: &[u8], entries: usize, limits: Limits) -> Result<Self, Error> {
        let mut input = Input::new(bytes, limits)?;
        if entries > limits.max_entries {
            return Err(Error::Limit("fsmonitor entries"));
        }

        let token = match input.word()? {
            1 => MonitorToken::Timestamp(u64::from_be_bytes(
                input
                    .take(8)?
                    .try_into()
                    .map_err(|_| invalid("truncated fsmonitor time"))?,
            )),
            2 => MonitorToken::Opaque(input.string()?.to_vec()),
            _ => return Err(invalid("unsupported fsmonitor version")),
        };

        let size = input.word()? as usize;
        let bitmap = input.take(size)?;
        let mut position = 0;
        let dirty = super::split::bitmap(bitmap, &mut position, entries)?;
        if position != bitmap.len() {
            return Err(invalid("trailing fsmonitor bitmap bytes"));
        }

        input.finish()?;

        Ok(Self {
            token,
            dirty,
            original: bytes.to_vec(),
        })
    }

    /// The checkpoint to pass to the monitor that produced this cache.
    pub fn token(&self) -> &MonitorToken {
        &self.token
    }

    /// Sorted index positions that were already dirty at the checkpoint.
    pub fn dirty(&self) -> &[usize] {
        &self.dirty
    }

    /// Returns the original validated representation, preserving its bitmap compression.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] if the payload exceeds `limits.max_bytes`.
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        Input::new(&self.original, limits)?;
        Ok(self.original.clone())
    }
}

/// A decoded untracked-cache directory, in depth-first order.
///
/// Names and untracked lists are byte strings. They have not passed platform path-safety checks.
/// The directory's cached stat words exist only when its valid bit is set; its exclude identity
/// exists only when the separate exclude-valid bit is set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrackedDirectory {
    name: Vec<u8>,
    children: usize,
    untracked: Vec<Vec<u8>>,
    valid: bool,
    check_only: bool,
    stat: Option<Stat>,
    exclude: Option<ObjectId>,
}
impl UntrackedDirectory {
    /// Directory name relative to its parent; the root name is empty.
    pub fn name(&self) -> &[u8] {
        &self.name
    }
    /// Number of immediate child directory blocks.
    pub fn children(&self) -> usize {
        self.children
    }
    /// Cached untracked names in Git's stored order, including trailing `/` directory markers.
    pub fn untracked(&self) -> &[Vec<u8>] {
        &self.untracked
    }
    /// Whether Git marked this directory's cached listing valid at the checkpoint.
    pub fn valid(&self) -> bool {
        self.valid
    }
    /// Whether Git scanned the directory only to determine if it contained an untracked path.
    pub fn check_only(&self) -> bool {
        self.check_only
    }
    /// Cached directory stat words; these do not prove current freshness.
    pub fn stat(&self) -> Option<Stat> {
        self.stat
    }
    /// Cached per-directory exclude-file identity, if present.
    ///
    /// Git can reuse an unchanged tracked ignore file's index blob identity. For a nonempty
    /// filesystem-loaded ignore file, it hashes the buffer with an additional newline, even
    /// when the file already ends in one. An empty buffer hashes the empty blob.
    pub fn exclude(&self) -> Option<ObjectId> {
        self.exclude
    }
}

/// An immutable decoded `UNTR` payload, retaining environment and validity claims.
///
/// Structural validity does not establish freshness. Consumers must compare the identity strings,
/// flags, ignore inputs and directory metadata before reusing any listing. Unknown directory flags
/// are retained as data. No filesystem access or index publication occurs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrackedCache {
    ident: Vec<u8>,
    exclude_stats: [Stat; 2],
    flags: u32,
    excludes: [ObjectId; 2],
    exclude_name: Vec<u8>,
    directories: Vec<UntrackedDirectory>,
    original: Vec<u8>,
}
impl UntrackedCache {
    /// Decodes a payload in the index's object format.
    ///
    /// Directory and aggregate untracked-name counts are bounded by `limits.max_entries`;
    /// individual strings by `limits.max_path_bytes`, and all input by `limits.max_bytes`.
    /// Traversal is iterative, so hostile depth cannot exhaust the call stack.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for truncated records, invalid tree framing or bitmaps, and
    /// [`Error::Limit`] for exceeded resource ceilings. Stat words and flags remain uninterpreted.
    pub fn parse(format: ObjectFormat, bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        let mut input = Input::new(bytes, limits)?;
        let ident_length = input.number()?;
        let ident = input.take(ident_length)?.to_vec();
        if ident
            .split(|byte| *byte == 0)
            .any(|string| string.len() > limits.max_path_bytes)
        {
            return Err(Error::Limit("untracked cache identity string"));
        }
        if !ident.is_empty() && ident.last() != Some(&0) {
            return Err(invalid("unterminated untracked-cache identity"));
        }

        let exclude_stats = [input.stat()?, input.stat()?];
        let flags = input.word()?;
        let excludes = [input.oid(format)?, input.oid(format)?];
        let exclude_name = input.string()?.to_vec();

        let count = input.number()?;
        if count > limits.max_entries {
            return Err(Error::Limit("untracked directories"));
        }

        let mut directories = Vec::new();
        let mut pending = usize::from(count != 0);
        let mut names = 0usize;

        for _ in 0..count {
            pending = pending
                .checked_sub(1)
                .ok_or(invalid("extra untracked directory"))?;
            let entries = input.number()?;
            names = names
                .checked_add(entries)
                .ok_or(Error::Limit("untracked names"))?;
            if names > limits.max_entries {
                return Err(Error::Limit("untracked names"));
            }

            let children = input.number()?;
            pending = pending
                .checked_add(children)
                .ok_or(Error::Limit("untracked directories"))?;
            if pending > count {
                return Err(invalid("untracked child count exceeds directories"));
            }

            let name = input.string()?.to_vec();
            let mut untracked = Vec::new();
            for _ in 0..entries {
                untracked.push(input.string()?.to_vec());
            }

            directories.push(UntrackedDirectory {
                name,
                children,
                untracked,
                valid: false,
                check_only: false,
                stat: None,
                exclude: None,
            });
        }

        if pending != 0 {
            return Err(invalid("missing untracked directory"));
        }

        if count != 0 {
            let valid = input.bitmap(count)?;
            let check_only = input.bitmap(count)?;
            let exclude_valid = input.bitmap(count)?;

            for position in &valid {
                let directory = directories
                    .get_mut(*position)
                    .ok_or(invalid("untracked directory bitmap position"))?;
                directory.valid = true;
                directory.stat = Some(input.stat()?);
            }

            for position in check_only {
                directories
                    .get_mut(position)
                    .ok_or(invalid("untracked directory bitmap position"))?
                    .check_only = true;
            }

            for position in exclude_valid {
                directories
                    .get_mut(position)
                    .ok_or(invalid("untracked directory bitmap position"))?
                    .exclude = Some(input.oid(format)?);
            }
        }

        if input.take(1)? != [0] {
            return Err(invalid("missing untracked cache terminator"));
        }

        input.finish()?;

        Ok(Self {
            ident,
            exclude_stats,
            flags,
            excludes,
            exclude_name,
            directories,
            original: bytes.to_vec(),
        })
    }
    /// NUL-separated environment identity strings, retaining their final NUL.
    pub fn ident(&self) -> &[u8] {
        &self.ident
    }
    /// Cached stats for `info/exclude` and `core.excludesFile`, respectively.
    pub fn exclude_stats(&self) -> &[Stat; 2] {
        &self.exclude_stats
    }
    /// Git's raw directory-walk flags; validity requires compatible scan policy.
    pub fn flags(&self) -> u32 {
        self.flags
    }
    /// Cached blob identities for the two base exclude files; null means absent.
    pub fn excludes(&self) -> &[ObjectId; 2] {
        &self.excludes
    }
    /// Per-directory exclude filename, usually `.gitignore`.
    pub fn exclude_name(&self) -> &[u8] {
        &self.exclude_name
    }
    /// Directory blocks in depth-first order.
    pub fn directories(&self) -> &[UntrackedDirectory] {
        &self.directories
    }
    /// Returns the validated original payload without changing bitmap compression or name order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] when the original payload exceeds `limits.max_bytes`.
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        Input::new(&self.original, limits)?;
        Ok(self.original.clone())
    }
}

impl Index {
    /// Decodes the optional UNTR extension without trusting its freshness claims.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`UntrackedCache::parse`], or [`Error::Malformed`] for duplicates.
    pub fn untracked_cache(&self, limits: Limits) -> Result<Option<UntrackedCache>, Error> {
        self.cache_payload(*b"UNTR")?
            .map(|bytes| UntrackedCache::parse(self.format, bytes, limits))
            .transpose()
    }
    /// Decodes the optional FSMN extension, bounding dirty bits by this index's entry count.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`FsmonitorCache::parse`], or [`Error::Malformed`] for duplicates.
    pub fn fsmonitor_cache(&self, limits: Limits) -> Result<Option<FsmonitorCache>, Error> {
        self.cache_payload(*b"FSMN")?
            .map(|bytes| FsmonitorCache::parse(bytes, self.entries.len(), limits))
            .transpose()
    }
    fn cache_payload(&self, signature: [u8; 4]) -> Result<Option<&[u8]>, Error> {
        let mut extensions = self
            .extensions
            .iter()
            .filter(|extension| extension.signature == signature);
        let first = extensions.next();
        if extensions.next().is_some() {
            return Err(invalid("duplicate status cache extension"));
        }
        Ok(first.map(|extension| extension.data.as_slice()))
    }
}

fn invalid(reason: &'static str) -> Error {
    Error::Malformed { offset: 0, reason }
}

struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
    limits: Limits,
}
impl<'a> Input<'a> {
    fn new(bytes: &'a [u8], limits: Limits) -> Result<Self, Error> {
        if bytes.len() > limits.max_bytes {
            return Err(Error::Limit("status cache bytes"));
        }

        Ok(Self {
            bytes,
            position: 0,
            limits,
        })
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(Error::Limit("status cache bytes"))?;
        let data = self
            .bytes
            .get(self.position..end)
            .ok_or(invalid("truncated status cache"))?;
        self.position = end;
        Ok(data)
    }
    fn word(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| invalid("truncated status cache word"))?,
        ))
    }
    fn string(&mut self) -> Result<&'a [u8], Error> {
        let remaining = self
            .bytes
            .get(self.position..)
            .ok_or(invalid("truncated cache string"))?;
        let length = remaining
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(invalid("unterminated cache string"))?;
        if length > self.limits.max_path_bytes {
            return Err(Error::Limit("status cache string"));
        }

        let string = self.take(length)?;
        self.take(1)?;
        Ok(string)
    }
    fn number(&mut self) -> Result<usize, Error> {
        let mut value = 0usize;
        loop {
            let byte = *self
                .take(1)?
                .first()
                .ok_or(invalid("truncated cache integer"))?;
            value = value
                .checked_mul(128)
                .and_then(|n| n.checked_add(usize::from(byte & 127)))
                .ok_or(Error::Limit("status cache integer"))?;
            if byte & 128 == 0 {
                return Ok(value);
            }
            value = value
                .checked_add(1)
                .ok_or(Error::Limit("status cache integer"))?;
        }
    }
    fn stat(&mut self) -> Result<Stat, Error> {
        Ok(Stat {
            ctime: Timestamp {
                seconds: self.word()?,
                nanoseconds: self.word()?,
            },
            mtime: Timestamp {
                seconds: self.word()?,
                nanoseconds: self.word()?,
            },
            device: self.word()?,
            inode: self.word()?,
            uid: self.word()?,
            gid: self.word()?,
            size: self.word()?,
        })
    }
    fn oid(&mut self, format: ObjectFormat) -> Result<ObjectId, Error> {
        ObjectId::from_bytes(format, self.take(format.digest_len())?)
            .map_err(|_| invalid("invalid cache object identity"))
    }
    fn bitmap(&mut self, count: usize) -> Result<Vec<usize>, Error> {
        super::split::bitmap(self.bytes, &mut self.position, count)
    }
    fn finish(self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid("trailing status cache bytes"))
        }
    }
}

impl Index {
    pub(super) fn set_untracked_cache_enabled(
        &mut self,
        enabled: bool,
        limits: Limits,
    ) -> Result<(), Error> {
        if let Some(extension) = self.extensions.iter().find(|extension| {
            !matches!(
                &extension.signature,
                b"TREE" | b"REUC" | b"sdir" | b"EOIE" | b"IEOT" | b"FSMN" | b"UNTR"
            )
        }) {
            return Err(Error::ExtensionPreventsEdit(extension.signature));
        }
        let present = self
            .extensions
            .iter()
            .any(|extension| extension.signature == *b"UNTR");
        if present == enabled {
            self.encoded_len(limits)?;
            return Ok(());
        }
        let mut replacement = self.clone();
        replacement
            .extensions
            .retain(|extension| !matches!(&extension.signature, b"UNTR" | b"EOIE" | b"IEOT"));
        if enabled {
            // Empty identity and no directory records request a fresh scan, as Git's
            // update-index --untracked-cache does before status fills the cache.
            let mut data = vec![0; 1 + 72];
            data.extend_from_slice(&6u32.to_be_bytes());
            data.resize(data.len() + 2 * self.format.digest_len(), 0);
            data.extend_from_slice(b".gitignore\0\0\0");
            UntrackedCache::parse(self.format, &data, limits)?;
            replacement.extensions.push(super::Extension {
                signature: *b"UNTR",
                data,
            });
        }
        replacement.original = None;
        replacement.encoded_len(limits)?;
        *self = replacement;
        Ok(())
    }
}
