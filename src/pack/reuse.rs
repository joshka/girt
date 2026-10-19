//! Pack writing that reuses stored entries and searches bounded deltas for transfer output.
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::AtomicBool;

use super::compression::{DeltaStats, PackCompression};
use super::file::{CopyError, FilePack};
use super::reader::entry_header;
use super::write::{
    Entry, Output, PackObject, PackWriteError, PackWriteLimits, PackWritten, entry_header_code,
    output_error, select_entry, validate, write_index, write_ordinary,
};
use crate::{IdSet, ObjectId, ObjectReadError, ReadLimits};

/// One retained object already stored at `position` in `packs[pack]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Packed {
    pub id: ObjectId,
    pub pack: usize,
    pub position: usize,
}

/// Failure while reading a source entry or writing the new pair.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReuseError {
    #[error("cannot read packed object {id}")]
    Read {
        id: ObjectId,
        #[source]
        source: ObjectReadError,
    },
    #[error(transparent)]
    Write(#[from] PackWriteError),
}

/// Writes a pack v2 and index v2 holding `packed` entries followed by `fresh` objects.
///
/// Packed entries keep their source order. A whole-object entry is copied byte for byte. A delta
/// entry is copied with its zlib stream unchanged when its base entry was written earlier from the
/// same source pack; its header is rewritten as an OFS_DELTA with the new distance. Any other delta
/// is reconstructed, identity-checked, and written whole. Legacy v1 indexes also use this
/// reconstruction path because they contain no entry CRC. Raw copies check source CRCs over
/// the bytes streamed, including original delta framing, without re-inflating. Fresh objects
/// are identity-checked and may become
/// OFS_DELTA entries against earlier fresh objects under `compression`. Identities in `fresh`
/// must not repeat identities in `packed`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_reusing(
    format: crate::ObjectFormat,
    packs: &[&FilePack],
    packed: &[Packed],
    fresh: &[PackObject<'_>],
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<PackWritten, ReuseError> {
    write_reusing_impl::<false>(
        format,
        packs,
        packed,
        fresh,
        None,
        None,
        outputs,
        limits,
        compression,
        read,
        read.0.max_delta_depth,
        DeltaFraming::Offset,
        check,
        &mut |_, _| {},
    )
}

/// How a copied delta names a base written earlier in the same pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeltaFraming {
    /// OFS_DELTA distances, as Git writes for fetch; the receiver must accept them.
    Offset,
    /// REF_DELTA identities, which every receive-pack accepts.
    Identity,
}

/// Transfer identities are hints from the source index. The receiver must derive them independently
/// by importing the finished pack before publishing references. No selected payloads are retained.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_transfer(
    store: &crate::Objects,
    ids: &[ObjectId],
    known_bases: &[(ObjectId, crate::ObjectKind)],
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    max_depth: usize,
    framing: DeltaFraming,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
    observe: &mut impl FnMut(u64, u64),
) -> Result<(PackWritten, IdSet), ReuseError> {
    write_transfer_controlled::<false>(
        store,
        ids,
        known_bases,
        outputs,
        limits,
        compression,
        read,
        max_depth,
        framing,
        check,
        observe,
    )
}

/// Writes a source transfer with compressed loose-input caps when explicitly controlled.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_transfer_controlled<const CAPPED: bool>(
    store: &crate::Objects,
    ids: &[ObjectId],
    known_bases: &[(ObjectId, crate::ObjectKind)],
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    max_depth: usize,
    framing: DeltaFraming,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
    observe: &mut impl FnMut(u64, u64),
) -> Result<(PackWritten, IdSet), ReuseError> {
    let max_depth = match compression {
        PackCompression::Delta(options) => max_depth.min(options.max_depth),
        PackCompression::Ordinary => max_depth,
    };
    // Store-assisted thin import can find a pending in-pack identity in receiver storage. Keep
    // every internal base ordinary so it resolves before external lookup can append that ID.
    let max_depth = if known_bases.is_empty() {
        max_depth
    } else {
        max_depth.min(1)
    };
    let packs = store.transfer_packs();
    let mut packed = Vec::new();
    let mut fresh = Vec::new();
    for &id in ids {
        check()?;
        // The copy below reads and limits each packed entry's header itself.
        let location = if CAPPED {
            store.transfer_location_capped(id, read.0, read.1)
        } else {
            store.transfer_location(id, read.0, read.1)
        }
        .map_err(|source| ReuseError::Read { id, source })?
        .ok_or(ReuseError::Read {
            id,
            source: ObjectReadError::Corrupt("missing transfer object"),
        })?;
        match location {
            Some(location) => packed.push(location),
            None => fresh.push(id),
        }
    }
    fresh.sort_unstable();
    let mut external = ExternalBases::<CAPPED>::new(store, known_bases);
    let written = write_reusing_impl::<CAPPED>(
        store.object_format(),
        &packs,
        &packed,
        &[],
        Some((store, &fresh)),
        Some(&mut external),
        outputs,
        limits,
        compression,
        read,
        max_depth,
        framing,
        check,
        observe,
    )?;
    Ok((written, external.used))
}

#[allow(clippy::too_many_arguments)]
fn write_reusing_impl<const CAPPED: bool>(
    format: crate::ObjectFormat,
    packs: &[&FilePack],
    packed: &[Packed],
    fresh: &[PackObject<'_>],
    stored: Option<(&crate::Objects, &[ObjectId])>,
    mut external: Option<&mut ExternalBases<'_, CAPPED>>,
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    max_depth: usize,
    framing: DeltaFraming,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
    observe: &mut impl FnMut(u64, u64),
) -> Result<PackWritten, ReuseError> {
    let (pack_sink, index_sink) = outputs;
    let (read_limits, cancelled) = read;
    let fresh = validate(format, fresh, limits, check)?;
    let mut order = Vec::with_capacity(packed.len());
    let mut seen = IdSet::with_capacity_and_hasher(packed.len(), Default::default());
    for item in packed {
        item.id
            .require_format(format)
            .map_err(PackWriteError::from)?;
        if seen.insert(item.id) {
            let source = packs[item.pack];
            // Copied entries keep the identity this index claims for them.
            let entry = source
                .verify_index()
                .and_then(|()| source.entry(item.position))
                .map_err(|source| ReuseError::Read {
                    id: item.id,
                    source,
                })?;
            order.push((item.pack, entry.offset, *item));
        }
    }
    for object in &fresh {
        if seen.contains(&object.id) {
            return Err(PackWriteError::ConflictingDuplicate(object.id).into());
        }
    }
    let fresh_count = stored.map_or(fresh.len(), |(_, ids)| ids.len());
    let count = u32::try_from(order.len() + fresh_count)
        .ok()
        .filter(|&count| count <= limits.max_objects)
        .ok_or(PackWriteError::Limit("object count"))?;
    order.sort_unstable_by_key(|&(pack, offset, _)| (pack, offset));
    check()?;

    let mut pack = Output::new(format, pack_sink, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut written: HashMap<(usize, usize), (u64, usize)> = HashMap::with_capacity(order.len());
    let mut deltas = DeltaStats::default();
    let mut input_bytes = fresh.iter().map(|o| o.data.len() as u64).sum::<u64>();
    observe(0, u64::from(count));
    let _ordered = super::HeaderScope::ordered();
    for &(source_index, _, item) in &order {
        check()?;
        let source = packs[source_index];
        let read_error = |source| ReuseError::Read {
            id: item.id,
            source,
        };
        let copy_error = |error| match error {
            CopyError::Read(source) => read_error(source),
            CopyError::Write(error) => ReuseError::Write(output_error(error)),
        };
        let entry = source.entry(item.position).map_err(read_error)?;
        let header = entry_header(source, &entry.span()).map_err(read_error)?;
        let (kind, length) = source
            .object_header(item.position, read_limits)
            .map_err(read_error)?;
        charge_input(&mut input_bytes, length, limits)?;
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let mut depth = 0;
        if entry.crc.is_none() {
            // Legacy v1 indexes have no entry CRC. Decode and verify instead of copying bytes
            // whose identity cannot be checked against the pinned source index while copying.
            let object = source
                .read(item.position, read_limits, cancelled)
                .map_err(read_error)?;
            depth = write_reconstructed(
                &mut pack,
                item.id,
                &object,
                external.as_deref_mut(),
                compression,
                read,
                max_depth,
                &mut input_bytes,
                limits,
                &mut deltas,
                check,
                |pack| write_ordinary(pack, object.kind(), object.data()).map_err(Into::into),
            )?;
        } else {
            match header.base {
                None if matches!(kind, crate::ObjectKind::Blob | crate::ObjectKind::Tree)
                    && external.as_ref().is_some_and(|bases| !bases.ids.is_empty())
                    && matches!(compression, PackCompression::Delta(options)
                        if max_depth > 0 && options.max_depth > 0 && length >= 64
                            && length <= options.max_object_bytes) =>
                {
                    let object = source
                        .read(item.position, read_limits, cancelled)
                        .map_err(read_error)?;
                    depth = write_reconstructed(
                        &mut pack,
                        item.id,
                        &object,
                        external.as_deref_mut(),
                        compression,
                        read,
                        max_depth,
                        &mut input_bytes,
                        limits,
                        &mut deltas,
                        check,
                        |pack| source.copy_raw(&entry, &header, pack).map_err(copy_error),
                    )?;
                }
                None => source
                    .copy_raw(&entry, &header, &mut pack)
                    .map_err(copy_error)?,
                Some(ref base) => match source
                    .base_position(base)
                    .map(|base| (base, written.get(&(source_index, base))))
                    .map_err(read_error)?
                {
                    (base, Some(&(base_offset, base_depth))) if base_depth < max_depth => {
                        depth = base_depth + 1;
                        if framing == DeltaFraming::Identity {
                            pack.put(&entry_header_code(7, header.length as u64))?;
                            pack.put(source.entry(base).map_err(read_error)?.id.as_bytes())?;
                        } else {
                            pack.put(&entry_header_code(6, header.length as u64))?;
                            pack.put(&offset_distance(offset - base_offset))?;
                        }
                        source
                            .copy_raw(&entry, &header, &mut pack)
                            .map_err(copy_error)?;
                        deltas.entries += 1;
                    }
                    _ => {
                        let object = source
                            .read(item.position, read_limits, cancelled)
                            .map_err(read_error)?;
                        if object.data().len() as u64 > limits.max_object_bytes {
                            return Err(PackWriteError::Limit("object bytes").into());
                        }
                        depth = write_reconstructed(
                            &mut pack,
                            item.id,
                            &object,
                            external.as_deref_mut(),
                            compression,
                            read,
                            max_depth,
                            &mut input_bytes,
                            limits,
                            &mut deltas,
                            check,
                            |pack| {
                                write_ordinary(pack, object.kind(), object.data())
                                    .map_err(Into::into)
                            },
                        )?;
                    }
                },
            }
        }
        deltas.max_depth = deltas.max_depth.max(depth);
        written.insert((source_index, item.position), (offset, depth));
        entries.push(Entry {
            id: item.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
        observe(entries.len() as u64, u64::from(count));
    }

    let mut depths = Vec::with_capacity(fresh.len());
    let mut fresh_offsets = Vec::with_capacity(fresh.len());
    let mut window = Vec::<(ObjectId, crate::Object, usize)>::new();
    for position in 0..fresh_count {
        check()?;
        let owned = if let Some((store, ids)) = stored {
            let id = ids[position];
            let object = if CAPPED {
                store.read_capped(id, read_limits, cancelled)
            } else {
                store.read_controlled(id, read_limits, cancelled)
            }
            .map_err(|source| ReuseError::Read { id, source })?
            .ok_or(ReuseError::Read {
                id,
                source: ObjectReadError::Corrupt("missing transfer object"),
            })?;
            charge_input(&mut input_bytes, object.data().len(), limits)?;
            Some((id, object))
        } else {
            None
        };
        let object = match &owned {
            Some((id, object)) => PackObject {
                id: *id,
                kind: object.kind(),
                data: object.data(),
            },
            None => fresh[position],
        };
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let mut search_stats = DeltaStats::default();
        let mut selected = match compression {
            PackCompression::Ordinary => None,
            PackCompression::Delta(mut options) => {
                if stored.is_some() {
                    options.max_depth = options.max_depth.min(max_depth);
                    let mut inputs: Vec<_> = window
                        .iter()
                        .map(|(id, object, _)| PackObject {
                            id: *id,
                            kind: object.kind(),
                            data: object.data(),
                        })
                        .collect();
                    inputs.push(object);
                    let depths: Vec<_> = window.iter().map(|(_, _, depth)| *depth).collect();
                    select_entry(
                        &inputs,
                        inputs.len() - 1,
                        &depths,
                        options,
                        &mut search_stats,
                        check,
                    )?
                } else {
                    select_entry(&fresh, position, &depths, options, &mut search_stats, check)?
                }
            }
        };
        if let (Some(external), PackCompression::Delta(mut options)) =
            (external.as_deref_mut(), compression)
        {
            options.max_depth = options.max_depth.min(max_depth);
            options.max_work = options.max_work.saturating_sub(search_stats.work);
            options.max_candidates = options
                .max_candidates
                .saturating_sub(search_stats.candidates as usize);
            selected = external.select(
                object,
                selected,
                options,
                read,
                &mut input_bytes,
                limits,
                &mut search_stats,
                check,
            )?;
        }
        deltas.candidates = deltas.candidates.saturating_add(search_stats.candidates);
        deltas.work = deltas.work.saturating_add(search_stats.work);
        if let Some(selected) = selected {
            if stored.is_some() {
                pack.put(&selected.bytes)?;
            } else if let Some(base) = selected.base {
                // Fresh deltas use the OFS framing that later repacks preserve verbatim.
                let header_end = selected.data_start - format.digest_len();
                let mut header = selected.bytes[..header_end].to_vec();
                header[0] = (header[0] & !0x70) | 0x60;
                pack.put(&header)?;
                pack.put(&offset_distance(offset - fresh_offsets[base]))?;
                pack.put(&selected.bytes[selected.data_start..])?;
            } else {
                pack.put(&selected.bytes)?;
            }
            if selected.depth != 0 {
                deltas.entries += 1;
                deltas.max_depth = deltas.max_depth.max(selected.depth);
            }
            depths.push(selected.depth);
        } else {
            write_ordinary(&mut pack, object.kind, object.data)?;
            depths.push(0);
        }
        let id = object.id;
        if let (Some((id, object)), PackCompression::Delta(options)) = (owned, compression) {
            if object.data().len() <= options.max_object_bytes && options.window > 0 {
                window.push((id, object, *depths.last().unwrap()));
                if window.len() > options.window {
                    window.remove(0);
                }
            } else {
                window.clear();
            }
        }
        fresh_offsets.push(offset);
        entries.push(Entry {
            id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
        observe(entries.len() as u64, u64::from(count));
    }
    check()?;
    let checksum = pack.finish()?;
    let pack_bytes = pack.bytes;
    entries.sort_unstable_by_key(|entry| entry.id);
    let mut index = Output::new(format, index_sink, limits.max_index_bytes, "index bytes");
    write_index(&entries, checksum, &mut index)?;
    index.finish()?;
    Ok(PackWritten {
        checksum,
        objects: count,
        pack_bytes,
        index_bytes: index.bytes,
        deltas,
    })
}

/// Only graph-proven receiver blobs and trees are offered here. Payloads are reconstructed through
/// normal lookup; metadata is cached, while at most one external payload is retained per search.
struct ExternalBases<'a, const CAPPED: bool> {
    store: &'a crate::Objects,
    ids: &'a [(ObjectId, crate::ObjectKind)],
    lengths: Vec<(crate::ObjectKind, usize, ObjectId)>,
    loaded: Vec<crate::ObjectKind>,
    used: IdSet,
}

impl<'a, const CAPPED: bool> ExternalBases<'a, CAPPED> {
    fn new(store: &'a crate::Objects, ids: &'a [(ObjectId, crate::ObjectKind)]) -> Self {
        Self {
            store,
            ids,
            lengths: Vec::new(),
            loaded: Vec::new(),
            used: IdSet::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn select(
        &mut self,
        object: PackObject<'_>,
        mut best: Option<super::write::SelectedEntry>,
        mut options: super::compression::DeltaOptions,
        read: (ReadLimits, &AtomicBool),
        input_bytes: &mut u64,
        limits: PackWriteLimits,
        stats: &mut DeltaStats,
        check: &mut impl FnMut() -> Result<(), PackWriteError>,
    ) -> Result<Option<super::write::SelectedEntry>, ReuseError> {
        if !matches!(
            object.kind,
            crate::ObjectKind::Blob | crate::ObjectKind::Tree
        ) || object.data.len() < 64
            || object.data.len() > options.max_object_bytes
            || options.max_depth == 0
            || options.window == 0
            || options.max_candidates == 0
            || options.max_work == 0
            || self.ids.is_empty()
        {
            return Ok(best);
        }
        if !self.loaded.contains(&object.kind) {
            self.loaded.push(object.kind);
            for &(id, kind) in self.ids {
                check()?;
                if kind != object.kind {
                    continue;
                }
                let metadata = if CAPPED {
                    self.store.transfer_object_capped(id, read.0, read.1)
                } else {
                    self.store.transfer_object(id, read.0, read.1)
                }
                .map_err(|source| ReuseError::Read { id, source })?;
                if let Some((actual, length, _)) = metadata {
                    if actual != kind {
                        return Err(ReuseError::Read {
                            id,
                            source: ObjectReadError::Corrupt("delta candidate kind"),
                        });
                    }
                    self.lengths.push((kind, length, id));
                }
            }
        }
        let mut chosen = None;
        let length = object.data.len();
        let mut candidates: Vec<_> = self
            .lengths
            .iter()
            .copied()
            .filter(|&(kind, size, id)| {
                kind == object.kind
                    && id != object.id
                    && size <= options.max_object_bytes
                    && size <= read.0.max_object_bytes
                    && size as u64 <= limits.max_object_bytes
                    && size >= length.div_ceil(2)
                    && size.div_ceil(2) <= length
            })
            .map(|(_, size, id)| (size, id))
            .collect();
        candidates.sort_unstable_by_key(|&(size, id)| (size.abs_diff(length), id));
        for (size, id) in candidates
            .into_iter()
            .take(options.max_candidates.min(options.window))
        {
            check()?;
            if options.max_work == 0 {
                break;
            }
            if size as u64 > limits.max_input_bytes.saturating_sub(*input_bytes) {
                continue;
            }
            charge_input(input_bytes, size, limits)?;
            let base = if CAPPED {
                self.store.read_capped(id, read.0, read.1)
            } else {
                self.store.read_controlled(id, read.0, read.1)
            }
            .map_err(|source| ReuseError::Read { id, source })?
            .ok_or(ReuseError::Read {
                id,
                source: ObjectReadError::Corrupt("missing delta candidate"),
            })?;
            if base.kind() != object.kind || base.data().len() != size {
                return Err(ReuseError::Read {
                    id,
                    source: ObjectReadError::Corrupt("delta candidate metadata changed"),
                });
            }
            let pair = [
                PackObject {
                    id,
                    kind: base.kind(),
                    data: base.data(),
                },
                object,
            ];
            let before = stats.work;
            let selected = select_entry(&pair, 1, &[0], options, stats, check)?;
            options.max_work = options.max_work.saturating_sub(stats.work - before);
            if let Some(selected) = selected
                && best
                    .as_ref()
                    .is_none_or(|entry| selected.bytes.len() < entry.bytes.len())
            {
                chosen = (selected.depth != 0).then_some(id);
                let strong_match = selected.depth != 0 && selected.bytes.len() <= length / 100;
                best = Some(selected);
                if strong_match {
                    break;
                }
            }
        }
        if let Some(id) = chosen {
            self.used.insert(id);
        }
        Ok(best)
    }
}

#[allow(clippy::too_many_arguments)]
fn write_reconstructed<W: Write, const CAPPED: bool>(
    pack: &mut Output<'_, W>,
    id: ObjectId,
    object: &crate::Object,
    external: Option<&mut ExternalBases<'_, CAPPED>>,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    max_depth: usize,
    input_bytes: &mut u64,
    limits: PackWriteLimits,
    stats: &mut DeltaStats,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
    whole: impl FnOnce(&mut Output<'_, W>) -> Result<(), ReuseError>,
) -> Result<usize, ReuseError> {
    let mut search_stats = DeltaStats::default();
    let selected =
        if let (Some(external), PackCompression::Delta(mut options)) = (external, compression) {
            options.max_depth = options.max_depth.min(max_depth);
            external.select(
                PackObject {
                    id,
                    kind: object.kind(),
                    data: object.data(),
                },
                None,
                options,
                read,
                input_bytes,
                limits,
                &mut search_stats,
                check,
            )?
        } else {
            None
        };
    stats.candidates = stats.candidates.saturating_add(search_stats.candidates);
    stats.work = stats.work.saturating_add(search_stats.work);
    if let Some(selected) = selected.filter(|entry| entry.depth != 0) {
        pack.put(&selected.bytes)?;
        if selected.depth != 0 {
            stats.entries += 1;
            stats.max_depth = stats.max_depth.max(selected.depth);
        }
        Ok(selected.depth)
    } else {
        whole(pack)?;
        Ok(0)
    }
}

fn charge_input(
    bytes: &mut u64,
    length: usize,
    limits: PackWriteLimits,
) -> Result<(), PackWriteError> {
    if length as u64 > limits.max_object_bytes {
        return Err(PackWriteError::Limit("object bytes"));
    }
    *bytes = bytes
        .checked_add(length as u64)
        .filter(|&n| n <= limits.max_input_bytes)
        .ok_or(PackWriteError::Limit("input bytes"))?;
    Ok(())
}

/// Encodes an OFS_DELTA base distance: big-endian 7-bit groups, each continuation biased by one.
fn offset_distance(mut distance: u64) -> Vec<u8> {
    let mut bytes = vec![(distance & 127) as u8];
    distance >>= 7;
    while distance != 0 {
        distance -= 1;
        bytes.push(128 | (distance & 127) as u8);
        distance >>= 7;
    }
    bytes.reverse();
    bytes
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;
    use crate::pack::write::{encode_index, write_pack_with_compression};
    use crate::pack::{DeltaOptions, PackCompression};
    use crate::{ObjectFormat, ObjectKind, PackLimits};

    fn versions() -> Vec<Vec<u8>> {
        let base: Vec<u8> = (0..4096u32)
            .flat_map(|n| format!("line {n}\n").into_bytes())
            .collect();
        (0..4u8)
            .map(|n| {
                let mut data = base.clone();
                data.extend_from_slice(&[b'v', b'0' + n, b'\n']);
                data
            })
            .collect()
    }

    fn open(root: &std::path::Path, name: &str, pack: &[u8], index: &[u8]) -> FilePack {
        let path = root.join(format!("{name}.idx"));
        std::fs::write(&path, index).unwrap();
        std::fs::write(path.with_extension("pack"), pack).unwrap();
        FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn rewrite(
        packs: &[&FilePack],
        packed: &[Packed],
        fresh: &[PackObject<'_>],
    ) -> Result<(PackWritten, Vec<u8>, Vec<u8>), ReuseError> {
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let written = write_reusing(
            ObjectFormat::Sha1,
            packs,
            packed,
            fresh,
            (&mut pack, &mut index),
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
            (ReadLimits::default(), &AtomicBool::new(false)),
            &mut || Ok(()),
        )?;
        Ok((written, pack, index))
    }

    fn source(root: &std::path::Path, data: &[Vec<u8>]) -> (FilePack, Vec<ObjectId>) {
        let objects: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let written = write_pack_with_compression(
            ObjectFormat::Sha1,
            &objects,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
        )
        .unwrap();
        assert!(written.deltas.entries > 0);
        (
            open(root, "source", &pack, &index),
            objects.iter().map(|o| o.id).collect(),
        )
    }

    fn packed(pack: &FilePack, ids: &[ObjectId]) -> Vec<Packed> {
        ids.iter()
            .map(|&id| Packed {
                id,
                pack: 0,
                position: pack.find(id).unwrap().unwrap(),
            })
            .collect()
    }

    fn assert_readable(root: &std::path::Path, pack: &[u8], index: &[u8], data: &[Vec<u8>]) {
        let output = open(root, "output", pack, index);
        for data in data {
            let id = ObjectId::for_blob(ObjectFormat::Sha1, data);
            let position = output.find(id).unwrap().unwrap();
            let object = output
                .read(position, ReadLimits::default(), &AtomicBool::new(false))
                .unwrap();
            assert_eq!(object.data(), data.as_slice());
        }
    }

    #[rstest::rstest]
    #[case::ordinary(0)]
    #[case::delta(1)]
    fn raw_reuse_rejects_source_crc_mismatch(#[case] damaged: usize) {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (_, ids) = source(root.path(), &data);
        let pack_bytes = std::fs::read(root.path().join("source.pack")).unwrap();
        let mut index = std::fs::read(root.path().join("source.idx")).unwrap();
        let count = ids.len();
        let identities = &index[1032..1032 + count * 20];
        let slot = identities
            .as_chunks::<20>()
            .0
            .iter()
            .position(|bytes| bytes.as_slice() == ids[damaged].as_bytes())
            .unwrap();
        index[1032 + count * 20 + slot * 4] ^= 1;
        let end = index.len() - 20;
        let checksum = ObjectFormat::Sha1.checksum(&index[..end]);
        index[end..].copy_from_slice(checksum.as_bytes());
        let pack = open(root.path(), "damaged", &pack_bytes, &index);
        let result = rewrite(&[&pack], &packed(&pack, &ids), &[]);
        assert!(
            matches!(result, Err(ReuseError::Read { id, source: ObjectReadError::Corrupt("entry CRC") }) if id == ids[damaged])
        );
    }

    /// Opening skips the index checksum, so reuse checks it before trusting the claimed names.
    #[rstest::rstest]
    #[case::checksum(None)]
    #[case::name(Some(19))]
    fn raw_reuse_rejects_index_checksum_mismatch(#[case] name_byte: Option<usize>) {
        let root = tempfile::tempdir().unwrap();
        let (_, _) = source(root.path(), &versions());
        let pack_bytes = std::fs::read(root.path().join("source.pack")).unwrap();
        let mut index = std::fs::read(root.path().join("source.idx")).unwrap();
        let damaged = match name_byte {
            Some(byte) => 1032 + byte,
            None => index.len() - 1,
        };
        index[damaged] ^= 1;
        let pack = open(root.path(), "damaged", &pack_bytes, &index);
        let ids: Vec<_> = pack.identities().map(Result::unwrap).collect();
        let result = rewrite(&[&pack], &packed(&pack, &ids), &[]);
        assert!(
            matches!(result, Err(ReuseError::Read { id, source: ObjectReadError::Corrupt("index checksum") }) if id == ids[0])
        );
    }

    #[test]
    fn transfer_writer_copies_compressed_entries_verbatim() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (source, ids) = source(root.path(), &data);
        let (_written, bytes, index) = rewrite(&[&source], &packed(&source, &ids), &[]).unwrap();
        let output = open(root.path(), "verbatim", &bytes, &index);
        let first = source.entry(source.at_offset(12).unwrap()).unwrap();
        let next = output.entry(output.at_offset(12).unwrap()).unwrap();
        let mut original = Vec::new();
        let mut copied = Vec::new();
        source
            .copy_raw(
                &first,
                &entry_header(&source, &first.span()).unwrap(),
                &mut original,
            )
            .unwrap();
        output
            .copy_raw(
                &next,
                &entry_header(&output, &next.span()).unwrap(),
                &mut copied,
            )
            .unwrap();
        assert_eq!(copied, original);
        assert_eq!(next.crc, Some(crc32fast::hash(&original)));
    }

    #[rstest::rstest]
    #[case::bytes(PackWriteLimits { max_input_bytes: 1, ..PackWriteLimits::default() })]
    #[case::object(PackWriteLimits { max_object_bytes: 1, ..PackWriteLimits::default() })]
    fn copied_entries_obey_payload_limits(#[case] limits: PackWriteLimits) {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (source, ids) = source(root.path(), &data);
        let result = write_reusing(
            ObjectFormat::Sha1,
            &[&source],
            &packed(&source, &ids),
            &[],
            (&mut Vec::new(), &mut Vec::new()),
            limits,
            PackCompression::Ordinary,
            (ReadLimits::default(), &AtomicBool::new(false)),
            &mut || Ok(()),
        );
        assert!(matches!(
            result,
            Err(ReuseError::Write(PackWriteError::Limit(_)))
        ));
    }

    #[test]
    fn copies_every_entry_and_keeps_deltas_when_bases_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &ids), &[]).unwrap();
        assert_eq!(written.objects, 4);
        assert_eq!(written.deltas.entries, 3);
        assert_eq!(written.deltas.candidates, 0);
        assert_readable(root.path(), &bytes, &index, &data);
    }

    #[test]
    fn rebuilds_delta_whose_base_is_not_kept() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let mut sorted = ids.clone();
        sorted.sort();
        // The writer orders by ID, so the smallest ID is a whole-object base.
        let kept: Vec<_> = sorted[1..].to_vec();
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &kept), &[]).unwrap();
        assert_eq!(written.objects, 3);
        let kept_data: Vec<_> = data
            .iter()
            .filter(|d| kept.contains(&ObjectId::for_blob(ObjectFormat::Sha1, d)))
            .cloned()
            .collect();
        assert_readable(root.path(), &bytes, &index, &kept_data);
    }

    #[test]
    fn fresh_objects_are_appended_and_may_delta_against_each_other() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data[..2]);
        let fresh: Vec<_> = data[2..]
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &ids), &fresh).unwrap();
        assert_eq!(written.objects, 4);
        assert_eq!(written.deltas.entries, 2);
        assert_readable(root.path(), &bytes, &index, &data);
    }

    #[test]
    fn rejects_fresh_object_that_repeats_a_packed_one() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let fresh = [PackObject {
            id: ids[0],
            kind: ObjectKind::Blob,
            data: &data[0],
        }];
        assert!(matches!(
            rewrite(&[&pack], &packed(&pack, &ids), &fresh),
            Err(ReuseError::Write(PackWriteError::ConflictingDuplicate(_)))
        ));
    }

    #[test]
    fn reports_delta_with_base_outside_its_pack() {
        let root = tempfile::tempdir().unwrap();
        let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"absent base");
        let mut entry = entry_header_code(7, 2);
        entry.extend_from_slice(missing.as_bytes());
        let mut encoder = ZlibEncoder::new(entry, Compression::new(6));
        encoder.write_all(&[11, 0]).unwrap();
        let entry = encoder.finish().unwrap();
        let mut pack = Vec::new();
        let mut output = Output::new(ObjectFormat::Sha1, &mut pack, u64::MAX, "pack bytes");
        output.put(b"PACK\0\0\0\x02\0\0\0\x01").unwrap();
        output.crc = crc32fast::Hasher::new();
        output.put(&entry).unwrap();
        let crc = output.crc.clone().finalize();
        let checksum = output.finish().unwrap();
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"thin target");
        let index = encode_index(
            &[Entry {
                id,
                offset: 12,
                crc,
            }],
            checksum,
        )
        .unwrap();
        let source = open(root.path(), "thin", &pack, &index);
        let result = rewrite(
            &[&source],
            &[Packed {
                id,
                pack: 0,
                position: 0,
            }],
            &[],
        );
        assert!(matches!(
            result,
            Err(ReuseError::Read {
                source: ObjectReadError::MissingBase(base),
                ..
            }) if base == missing
        ));
    }

    #[test]
    fn offset_distance_uses_biased_continuation_groups() {
        assert_eq!(offset_distance(1), [1]);
        assert_eq!(offset_distance(127), [127]);
        assert_eq!(offset_distance(128), [0x80, 0]);
        assert_eq!(offset_distance(16511), [0xff, 0x7f]);
        assert_eq!(offset_distance(16512), [0x80, 0x80, 0]);
    }
    #[rstest::rstest]
    #[case::one(1)]
    #[case::largest_single_byte(127)]
    #[case::two_bytes(128)]
    #[case::three_bytes(16512)]
    #[case::largest(u64::MAX)]
    fn offset_encoding_round_trips(#[case] distance: u64) {
        let bytes = offset_distance(distance);
        let mut input = bytes.as_slice();
        let mut next = super::super::delta::byte(&mut input).unwrap();
        let mut actual = u64::from(next & 127);
        while next & 128 != 0 {
            next = super::super::delta::byte(&mut input).unwrap();
            actual = (actual + 1) * 128 + u64::from(next & 127);
        }
        assert_eq!(actual, distance);
        assert!(input.is_empty());
    }
    // Windows rejects the writer at open; file::publication_tests covers that boundary.
    #[cfg(unix)]
    #[test]
    fn pinned_source_crc_is_checked_during_copy() {
        use std::io::{Seek as _, SeekFrom};
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (source, ids) = source(root.path(), &data);
        let first = source.entry(source.at_offset(12).unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("source.pack"))
            .unwrap();
        file.seek(SeekFrom::Start((first.end - 1) as u64)).unwrap();
        let bytes = std::fs::read(root.path().join("source.pack")).unwrap();
        file.write_all(&[bytes[first.end - 1] ^ 128]).unwrap();
        assert!(rewrite(&[&source], &packed(&source, &ids), &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn truncation_of_pinned_source_is_rejected_during_copy() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (source, ids) = source(root.path(), &data);
        let selected = packed(&source, &ids);
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("source.pack"))
            .unwrap()
            .set_len(16)
            .unwrap();
        assert!(rewrite(&[&source], &selected, &[]).is_err());
    }
}

#[cfg(test)]
mod capped_transfer_tests {
    use super::*;
    use crate::{AlternateLimits, LooseObjects, ObjectFormat, Objects, PackLimits};

    struct Source {
        root: tempfile::TempDir,
        objects: Objects,
        first: ObjectId,
        victim: ObjectId,
        padded: Vec<u8>,
    }

    impl Source {
        fn new(format: ObjectFormat) -> Self {
            let root = tempfile::tempdir().unwrap();
            let loose = LooseObjects::new(root.path(), format);
            let victim = loose.write_blob(b"").unwrap();
            // Fresh transfer entries sort by identity. Put one ordinary entry before the victim
            // so the writer observer can replace that victim after location classification.
            let first = (0_u32..100_000)
                .find_map(|n| {
                    let id = loose.write_blob(&n.to_be_bytes()).unwrap();
                    (id < victim).then_some(id)
                })
                .unwrap();
            let padded = crate::zlib_fixture::padded(b"blob 0\0");
            let objects = Objects::open(
                format,
                root.path(),
                PackLimits::default(),
                AlternateLimits::default(),
            )
            .unwrap();
            Self {
                root,
                objects,
                first,
                victim,
                padded,
            }
        }

        fn replace_victim(&self) {
            let hex = self.victim.to_string();
            let path = self.root.path().join(&hex[..2]).join(&hex[2..]);
            std::fs::rename(&path, self.root.path().join("original-victim")).unwrap();
            std::fs::write(path, &self.padded).unwrap();
        }

        fn write(
            &self,
            ids: &[ObjectId],
            cancel: &AtomicBool,
            observe: &mut impl FnMut(u64, u64),
        ) -> Result<PackWritten, ReuseError> {
            write_transfer_controlled::<true>(
                &self.objects,
                ids,
                &[],
                (&mut Vec::new(), &mut Vec::new()),
                PackWriteLimits::default(),
                PackCompression::Ordinary,
                (
                    ReadLimits::default().with_max_input_bytes(self.padded.len() - 1),
                    cancel,
                ),
                50,
                DeltaFraming::Offset,
                &mut || Ok(()),
                observe,
            )
            .map(|(written, _)| written)
        }
    }

    #[rstest::rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn capped_transfer_refuses_padded_location_input(#[case] format: ObjectFormat) {
        let source = Source::new(format);
        source.replace_victim();
        let result = source.write(&[source.victim], &AtomicBool::new(false), &mut |_, _| {});
        assert!(matches!(
            result,
            Err(ReuseError::Read {
                source: ObjectReadError::Limit("loose compressed bytes"),
                ..
            })
        ));
    }

    #[rstest::rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn capped_transfer_rechecks_fresh_input_after_classification(#[case] format: ObjectFormat) {
        let source = Source::new(format);
        let result = source.write(
            &[source.first, source.victim],
            &AtomicBool::new(false),
            &mut |done, _| {
                if done == 1 {
                    source.replace_victim();
                }
            },
        );
        assert!(matches!(
            result,
            Err(ReuseError::Read {
                source: ObjectReadError::Limit("loose compressed bytes"),
                ..
            })
        ));
    }

    #[rstest::rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn capped_transfer_observes_cancelled_source_reads(#[case] format: ObjectFormat) {
        let source = Source::new(format);
        let result = source.write(&[source.victim], &AtomicBool::new(true), &mut |_, _| {});
        assert!(matches!(
            result,
            Err(ReuseError::Read {
                source: ObjectReadError::Cancelled,
                ..
            })
        ));
    }
}
