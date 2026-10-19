//! Encode a primary store's verified pack indexes without modifying storage.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{ObjectId, ObjectReadError, Objects};

/// Duplicate-object selection when encoding a multi-pack index.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Prefer this pack checksum whenever that pack contains a duplicate object.
    /// Otherwise the most recently modified pack wins; filename order breaks ties.
    pub preferred_pack: Option<ObjectId>,
}

impl WriteOptions {
    /// Chooses a pack checksum to prefer for duplicate objects.
    #[must_use]
    pub fn with_preferred_pack(mut self, pack: ObjectId) -> Self {
        self.preferred_pack = Some(pack);
        self
    }
}

/// Work and output bounds for multi-pack index encoding.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct WriteLimits {
    /// Maximum input object occurrences, including duplicates (default 5 million).
    /// Working memory is proportional to this count, independently of final output size.
    pub max_objects: usize,
    /// Maximum encoded file size (default 64 MiB).
    pub max_bytes: usize,
    /// Maximum named packs (default 4096).
    pub max_packs: usize,
}

impl Default for WriteLimits {
    fn default() -> Self {
        Self {
            max_objects: 5_000_000,
            max_bytes: 64 * 1024 * 1024,
            max_packs: 4096,
        }
    }
}

/// A multi-pack index could not be encoded from the selected snapshot.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteError {
    /// No indexed objects are available; Git verification requires a nonempty object table.
    #[error("no packed objects to index")]
    NoObjects,
    /// Source indexes failed structural, checksum or generation verification.
    #[error("reading multi-pack index input")]
    Read(#[from] ObjectReadError),
    /// A caller-selected bound would be exceeded.
    #[error("multi-pack index writing limit exceeded: {0}")]
    Limit(&'static str),
    /// The preferred pack is not part of this primary store.
    #[error("preferred pack {0} is unavailable")]
    PreferredPack(ObjectId),
    /// An input filename cannot be represented by the supported MIDX format.
    #[error("unsupported multi-pack index pack name: {0}")]
    PackName(std::path::PathBuf),
    /// A cooperating MIDX writer holds this lock, or left it after an interrupted operation.
    /// Remove the lock only after establishing that no writer owns it.
    #[error("multi-pack index writer lock already exists: {0}")]
    Locked(std::path::PathBuf),
    /// The caller requested cancellation; no artifact is returned.
    #[error("multi-pack index writing cancelled")]
    Cancelled,
    /// Filesystem metadata needed to choose among duplicate copies is unavailable.
    #[error("reading multi-pack index input metadata")]
    Io(#[from] std::io::Error),
}

impl Objects {
    /// Encodes a monolithic v1 MIDX for the primary store with default bounds.
    ///
    /// See [`Self::encode_multi_pack_index_with`] for source validation and publication contracts.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError`] for invalid sources, selection or exhausted bounds.
    ///
    /// ```no_run
    /// use girt::Repository;
    /// use girt::multi_pack_index::{MultiPackIndex, WriteOptions};
    /// let repository = Repository::open("repo.git")?;
    /// let objects = repository.objects()?;
    /// let mut options = WriteOptions::default();
    /// options.preferred_pack = objects
    ///     .stores()
    ///     .next()
    ///     .and_then(|store| store.packs().next())
    ///     .map(|pack| pack.checksum());
    /// let bytes = objects.encode_multi_pack_index(options)?;
    /// let index = MultiPackIndex::parse(&bytes, repository.object_format())?;
    /// println!("{} unique packed objects", index.len());
    /// // Keep these bytes private until installation can exclude repacking and pruning.
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn encode_multi_pack_index(&self, options: WriteOptions) -> Result<Vec<u8>, WriteError> {
        self.encode_multi_pack_index_with(options, WriteLimits::default(), &AtomicBool::new(false))
    }

    /// Encodes verified primary-store indexes into caller-owned bytes without filesystem mutation.
    ///
    /// Alternate packs are excluded. Every source index's checksum and structure are checked before
    /// its identities are copied. Duplicate objects use `options`; sorted IDs and names make output
    /// deterministic for the same inputs. Pack payloads are not scanned. The caller must exclude
    /// repacking or pruning while constructing and installing an artifact; a retained snapshot can
    /// otherwise name packs already removed from the directory. Source generation changes refuse
    /// encoding. Use the repository publication API for coordinated installation.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError`] for changed or damaged indexes, unknown preferred packs, unsupported
    /// filenames, cancellation or exhausted input/output bounds. No partial bytes are returned.
    pub fn encode_multi_pack_index_with(
        &self,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, WriteError> {
        self.encode_multi_pack_index_selected(options, limits, cancel, |_| true)
    }

    // Expiry can publish the existing encoder's format while excluding its verified deletion set.
    // The public encoder still selects every primary pack and retains all its validation.
    pub(crate) fn encode_multi_pack_index_selected(
        &self,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
        include: impl Fn(&Path) -> bool,
    ) -> Result<Vec<u8>, WriteError> {
        check(cancel)?;
        let mut packs = self.primary_packs_with_paths();
        packs.retain(|(path, _)| include(path));
        packs.sort_unstable_by_key(|(path, _)| *path);
        if packs.len() > limits.max_packs || packs.len() > u32::MAX as usize {
            return Err(WriteError::Limit("packs"));
        }
        if let Some(id) = options.preferred_pack
            && !packs.iter().any(|(_, pack)| pack.checksum() == id)
        {
            return Err(WriteError::PreferredPack(id));
        }
        let mut count = 0_usize;
        let mut names = Vec::new();
        let mut ranks = Vec::new();
        for (position, (path, pack)) in packs.iter().enumerate() {
            check(cancel)?;
            count = count
                .checked_add(pack.object_count())
                .ok_or(WriteError::Limit("objects"))?;
            if count > limits.max_objects || count > u32::MAX as usize {
                return Err(WriteError::Limit("objects"));
            }
            pack.verify_index()?;
            pack.check_unchanged()?;
            let name = index_name(path)?;
            if names
                .len()
                .checked_add(name.len() + 1)
                .is_none_or(|size| size > limits.max_bytes)
            {
                return Err(WriteError::Limit("bytes"));
            }
            names.extend_from_slice(name.as_bytes());
            names.push(0);
            let modified = std::fs::metadata(path)?.modified()?;
            ranks.push((
                options.preferred_pack == Some(pack.checksum()),
                modified,
                usize::MAX - position,
            ));
        }
        if count == 0 {
            return Err(WriteError::NoObjects);
        }
        while names.len() % 4 != 0 {
            names.push(0);
        }
        if names.len() > limits.max_bytes {
            return Err(WriteError::Limit("bytes"));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| WriteError::Limit("working storage"))?;
        for (ordinal, (_, pack)) in packs.iter().enumerate() {
            for position in 0..pack.object_count() {
                check(cancel)?;
                entries.push((
                    pack.identity_at(position),
                    ordinal,
                    pack.offset(position)? as u64,
                ));
            }
        }
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| ranks[b.1].cmp(&ranks[a.1])));
        entries.dedup_by_key(|entry| entry.0);
        check(cancel)?;
        let bytes = encode(
            self.object_format(),
            packs.len(),
            names,
            &entries,
            limits.max_bytes,
        )?;
        for (_, pack) in &packs {
            check(cancel)?;
            pack.check_unchanged()?;
        }
        Ok(bytes)
    }
}

fn index_name(path: &Path) -> Result<String, WriteError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(WriteError::PackName(path.to_owned()))?;
    let stem = name
        .strip_suffix(".pack")
        .filter(|name| name.starts_with("pack-") || name.starts_with("loose-"))
        .ok_or(WriteError::PackName(path.to_owned()))?;
    Ok(format!("{stem}.idx"))
}

fn encode(
    format: crate::ObjectFormat,
    packs: usize,
    names: Vec<u8>,
    entries: &[(ObjectId, usize, u64)],
    max_bytes: usize,
) -> Result<Vec<u8>, WriteError> {
    let has_large = entries.iter().any(|entry| entry.2 > u64::from(u32::MAX));
    let large_count = if has_large {
        entries.iter().filter(|entry| entry.2 > 0x7fff_ffff).count()
    } else {
        0
    };
    let count = if has_large { 5 } else { 4 };
    let table_end = 12 + (count + 1) * 12;
    let ids_bytes = entries
        .len()
        .checked_mul(format.digest_len())
        .ok_or(WriteError::Limit("bytes"))?;
    let offset_bytes = entries
        .len()
        .checked_mul(8)
        .ok_or(WriteError::Limit("bytes"))?;
    let large_bytes = large_count
        .checked_mul(8)
        .filter(|_| large_count <= 0x8000_0000)
        .ok_or(WriteError::Limit("large offsets"))?;
    let sizes = [names.len(), 1024, ids_bytes, offset_bytes, large_bytes];
    let total = sizes
        .iter()
        .try_fold(table_end + format.digest_len(), |sum, size| {
            sum.checked_add(*size)
        })
        .ok_or(WriteError::Limit("bytes"))?;
    if total > max_bytes {
        return Err(WriteError::Limit("bytes"));
    }
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(b"MIDX");
    bytes.extend_from_slice(&[
        1,
        if format == crate::ObjectFormat::Sha1 {
            1
        } else {
            2
        },
        count as u8,
        0,
    ]);
    bytes.extend_from_slice(&(packs as u32).to_be_bytes());
    let mut offset = table_end;
    for (tag, size) in [*b"PNAM", *b"OIDF", *b"OIDL", *b"OOFF", *b"LOFF"]
        .iter()
        .zip(sizes)
        .take(count)
    {
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(&(offset as u64).to_be_bytes());
        offset += size;
    }
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(offset as u64).to_be_bytes());
    bytes.extend_from_slice(&names);
    let mut fanout = [0_u32; 256];
    for entry in entries {
        fanout[usize::from(entry.0.as_bytes()[0])] += 1;
    }
    let mut total = 0_u32;
    for count in fanout {
        total += count;
        bytes.extend_from_slice(&total.to_be_bytes());
    }
    for entry in entries {
        bytes.extend_from_slice(entry.0.as_bytes());
    }
    let mut large = Vec::with_capacity(large_bytes);
    for &(_, pack, offset) in entries {
        bytes.extend_from_slice(&(pack as u32).to_be_bytes());
        let offset = if has_large && offset > 0x7fff_ffff {
            let slot = (large.len() / 8) as u32;
            large.extend_from_slice(&offset.to_be_bytes());
            0x8000_0000 | slot
        } else {
            offset as u32
        };
        bytes.extend_from_slice(&offset.to_be_bytes());
    }
    bytes.extend_from_slice(&large);
    let checksum = format.checksum(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

pub(super) fn check(cancel: &AtomicBool) -> Result<(), WriteError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(WriteError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectFormat;
    use crate::multi_pack_index::MultiPackIndex;

    #[test]
    fn large_offset_encoding_preserves_high_direct_values_and_full_width_slots() {
        let format = ObjectFormat::Sha1;
        let mut entries = vec![
            (ObjectId::for_blob(format, b"large-a"), 0, 0x8000_0000),
            (ObjectId::for_blob(format, b"large-b"), 0, 0x1_0000_0000),
        ];
        entries.sort_unstable_by_key(|entry| entry.0);
        let bytes = encode(
            format,
            1,
            b"pack-original.idx\0\0\0".to_vec(),
            &entries,
            4096,
        )
        .unwrap();
        let index = MultiPackIndex::parse(&bytes, format).unwrap();
        assert_eq!(
            index
                .find(ObjectId::for_blob(format, b"large-a"))
                .unwrap()
                .offset,
            0x8000_0000
        );
        assert_eq!(
            index
                .find(ObjectId::for_blob(format, b"large-b"))
                .unwrap()
                .offset,
            0x1_0000_0000
        );
    }
}
