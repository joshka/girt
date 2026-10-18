//! Native local advertisement and reachable-pack construction.

use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::num::NonZeroU32;
use std::ops::ControlFlow;

use super::{
    AdvertisedRef, Advertisement, FetchError, FetchLimits, KnownHistory, LocalFetchProgress,
    NativeContents, ReceivedFetch,
};
use crate::refs::{RefName, Target};
use crate::transport::TransportControl;
use crate::{IdMap, IdSet, ObjectId, ObjectKind, PackWriteLimits, Repository, UnverifiedObject};

#[cfg(test)]
pub(super) fn receive(
    source: &Repository,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    observe: impl FnMut(LocalFetchProgress),
) -> Result<ReceivedFetch, FetchError> {
    receive_depth(
        source, select, known, None, limits, control, progress, observe,
    )
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "local transfer keeps explicit selection and progress policy"
)]
pub(super) fn receive_depth(
    source: &Repository,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    observe: impl FnMut(LocalFetchProgress),
) -> Result<ReceivedFetch, FetchError> {
    receive_depth_observed(
        source,
        select,
        known,
        depth,
        limits,
        control,
        progress,
        observe,
        |_| {},
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "local transfer keeps explicit progress policy"
)]
pub(super) fn receive_depth_observed(
    source: &Repository,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    mut progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    mut observe: impl FnMut(LocalFetchProgress),
    mut validation: impl FnMut(super::ValidationProgress),
) -> Result<ReceivedFetch, FetchError> {
    check(control)?;
    if depth.is_some_and(|depth| depth.get() != 1) {
        return Err(FetchError::Unsupported("native local depth beyond one"));
    }
    if depth.is_some() && !known.shallow.is_empty() {
        return Err(FetchError::Unsupported("native local shallow refresh"));
    }
    if !source.shallow_roots().is_empty() {
        return Err(FetchError::Unsupported("shallow local source"));
    }
    let store = source
        .objects_with(limits.source_packs)
        .map_err(FetchError::SourceObjects)?;
    let advertisement = advertise_store(source, &store, limits, control)?;
    let mut wants = select(&advertisement);
    if wants.len() > limits.max_wants {
        return Err(FetchError::Limit("wants"));
    }
    let mut seen = IdSet::default();
    wants.retain(|id| seen.insert(*id));
    if depth.is_some() && wants.len() != 1 {
        return Err(FetchError::Unsupported("native local depth selection"));
    }
    let empty = KnownHistory::default();
    let known = if known.applies_to(&wants) {
        known
    } else {
        &empty
    };
    let offered: IdSet = advertisement
        .refs
        .iter()
        .map(|reference| reference.id)
        .collect();
    for id in &wants {
        if !offered.contains(id) {
            return Err(FetchError::Unadvertised(*id));
        }
        if id.format() != source.object_format() {
            return Err(FetchError::Unsupported("local object format"));
        }
    }
    observe(LocalFetchProgress::Reading { objects: 0 });
    check(control)?;
    if wants.is_empty() {
        observe(LocalFetchProgress::Complete);
        return Ok(ReceivedFetch::native(
            advertisement,
            wants,
            NativeContents {
                format: source.object_format(),
                pack: Vec::new().into(),
                index: Vec::new(),
                checksum: None,
                objects: 0,
                dependencies: Vec::new(),
                complete: IdMap::default(),
                shallow: Vec::new(),
                limits,
            },
        ));
    }
    let mut pending: VecDeque<_> = wants.iter().copied().map(|id| (id, None)).collect();
    // Commits' root trees wait until every queued commit is read, then go oldest first. A chain
    // of new commits then reaches the one with a complete parent, whose paired tree marks the
    // unchanged entries the later trees share, before any later tree is walked.
    let mut commit_trees = Vec::new();
    // Each object's actual kind once read. Every edge naming it must require that kind.
    let mut observed = IdMap::<ObjectKind>::default();
    // The kind the first edge required of each object queued but not yet read. Trees name the
    // same objects many times before the walk reaches them; queueing each once keeps the queue
    // proportional to the objects rather than to the tree entries (tens of millions on git/git).
    let mut queued = IdMap::<ObjectKind>::default();
    let mut selected = Vec::<ObjectId>::new();
    let mut dependencies = Vec::new();
    let mut boundaries = super::boundaries::TreeBoundaries::new(known, limits);
    let mut shallow = Vec::new();
    let mut bytes = 0_usize;
    let mut edges = 0_usize;
    let mut read = limits.known_read;
    read.max_object_bytes = read.max_object_bytes.min(limits.max_object_bytes);
    if let Some(ids) = bitmap_selection(&store, &wants, known, depth, limits, control)? {
        selected = ids;
        observe(LocalFetchProgress::Reading {
            objects: selected.len() as u64,
        });
    } else {
        prefetch::with_prefetch(
            &store,
            read,
            control.cancel,
            limits.max_threads,
            |prefetch| {
                while let Some((id, kind)) = pending.pop_front().or_else(|| {
                    pending.extend(commit_trees.drain(..).rev());
                    pending.pop_front()
                }) {
                    prefetch.popped();
                    check(control)?;
                    queued.remove(&id);
                    if let Some(&actual) = observed.get(&id) {
                        if kind.is_some_and(|kind| kind != actual) {
                            return Err(FetchError::Kind(id));
                        }
                        continue;
                    }
                    if observed.len() >= limits.max_objects {
                        return Err(FetchError::Limit("local objects"));
                    }
                    let prefetch::Fetched {
                        kind: object_kind,
                        length,
                        payload,
                    } = prefetch.take(id, kind, &pending, &observed)?;
                    if kind.is_some_and(|kind| kind != object_kind) {
                        return Err(FetchError::Kind(id));
                    }
                    observed.insert(id, object_kind);
                    // Readers synthesize the empty tree even without a Git object file. Materialize
                    // it whenever this walk reaches it, rather than treating it
                    // as stored destination data.
                    let available = depth.is_none()
                        && limits.max_haves > 0
                        && known.kind(id) == Some(object_kind)
                        && !(object_kind == ObjectKind::Tree && length == 0);
                    if available
                        && known.complete_kind(id) == Some(object_kind)
                        && !known.shallow.contains(&id)
                    {
                        dependencies.push(id);
                        continue;
                    }
                    if depth.is_some() && id == wants[0] && object_kind != ObjectKind::Commit {
                        return Err(FetchError::Unsupported(
                            "native local depth requires commit tip",
                        ));
                    }
                    bytes = bytes
                        .checked_add(length)
                        .ok_or(FetchError::Limit("local bytes"))?;
                    if bytes > limits.max_decode_bytes {
                        return Err(FetchError::Limit("local bytes"));
                    }
                    // Blobs have no edges. Their framing is checked now and their identity during
                    // import. Selection only follows edges: every selected object's
                    // identity is checked when the destination imports it, so this walk
                    // does not hash payloads it never returns.
                    let object = if object_kind == ObjectKind::Blob {
                        UnverifiedObject {
                            kind: object_kind,
                            data: Default::default(),
                        }
                    } else if let Some(object) = payload {
                        object
                    } else {
                        store
                            .read_unverified(id, read, control.cancel)
                            .map_err(|source| FetchError::LocalRead { id, source })?
                            .ok_or(FetchError::Missing(id))?
                    };
                    if depth.is_none() {
                        match object_kind {
                            ObjectKind::Commit => {
                                if let Ok(commit) =
                                    crate::Commit::parse(source.object_format(), object.data())
                                {
                                    boundaries.pair_commit(known, commit.tree(), commit.parents());
                                }
                            }
                            ObjectKind::Tree => {
                                boundaries.pair_tree(known, id, object.data());
                            }
                            _ => {}
                        }
                    }
                    crate::edges::visit(id, object.kind(), object.data(), |target, kind| {
                        check(control)?;
                        if depth.is_some() && id == wants[0] && kind == ObjectKind::Commit {
                            // Depth one cuts only the selected commit's parent edges. Its tree and
                            // all reachable content stay in the
                            // self-contained pack.
                            if shallow.is_empty() {
                                shallow.push(id);
                            }
                            return Ok(());
                        }
                        edges += 1;
                        if edges > limits.max_connectivity_edges {
                            return Err(FetchError::Limit("local edges"));
                        }
                        // Most edges name an object already read. Check it now rather than queueing
                        // it, as trees repeat their unchanged entries
                        // across a history.
                        if let Some(&actual) = observed.get(&target) {
                            return if actual == kind {
                                Ok(())
                            } else {
                                Err(FetchError::Kind(target))
                            };
                        }
                        // A complete parent's tree names this object. Send it again only if the
                        // destination lost it.
                        if boundaries.kind(target) == Some(kind)
                            && known.stored_kind(target) == Some(kind)
                        {
                            observed.insert(target, kind);
                            dependencies.push(target);
                            return Ok(());
                        }
                        // An object queued once is read once, and its read checks the first
                        // required kind. Another edge requiring a different kind cannot match it.
                        match queued.entry(target) {
                            Entry::Occupied(entry) => {
                                return if *entry.get() == kind {
                                    Ok(())
                                } else {
                                    Err(FetchError::Kind(target))
                                };
                            }
                            Entry::Vacant(entry) => {
                                entry.insert(kind);
                            }
                        }
                        if object_kind == ObjectKind::Commit && kind == ObjectKind::Tree {
                            commit_trees.push((target, Some(kind)));
                        } else {
                            pending.push_back((target, Some(kind)));
                        }
                        Ok(())
                    })?;
                    if available {
                        // An interrupted unpack can leave this object without its descendants.
                        // Walking its source edges above proves those
                        // descendants are either selected or complete.
                        dependencies.push(id);
                    } else {
                        selected.push(id);
                        observe(LocalFetchProgress::Reading {
                            objects: selected.len() as u64,
                        });
                    }
                    check(control)?;
                }
                Ok::<_, FetchError>(())
            },
        )?;
    }
    // The walk's tables are proportional to the whole history; release them before writing.
    drop((pending, commit_trees, observed, queued));
    check(control)?;
    if progress(&[]) == ControlFlow::Break(()) {
        return Err(FetchError::Cancelled);
    }
    check(control)?;
    if selected.is_empty() {
        observe(LocalFetchProgress::Complete);
        return Ok(ReceivedFetch::native(
            advertisement,
            wants,
            NativeContents {
                format: source.object_format(),
                pack: Vec::new().into(),
                index: Vec::new(),
                checksum: None,
                objects: 0,
                complete: dependencies
                    .iter()
                    .filter_map(|&id| {
                        Some((
                            id,
                            super::boundaries::complete_kind(known, &boundaries, id)?,
                        ))
                    })
                    .collect(),
                dependencies,
                shallow,
                limits,
            },
        ));
    }
    let mut temporary = tempfile::NamedTempFile::new()?;
    let mut index = Vec::new();
    let written = {
        let mut sink = std::io::BufWriter::with_capacity(64 * 1024, temporary.as_file_mut());
        let (written, _) = crate::pack::write_transfer(
            &store,
            &selected,
            &[],
            (&mut sink, &mut index),
            PackWriteLimits {
                max_objects: limits.max_objects.try_into().unwrap_or(u32::MAX),
                max_object_bytes: limits.max_object_bytes as u64,
                max_input_bytes: limits.max_decode_bytes as u64,
                max_pack_bytes: limits.max_pack_bytes as u64,
                // The index size follows from the object count: header, fanout and trailer, plus at
                // most a 32-byte ID, CRC, offset and large offset per object. `max_objects` bounds
                // it, so a fixed index cap would only refuse sources within every other bound.
                max_index_bytes: (limits.max_objects as u64)
                    .saturating_mul(48)
                    .saturating_add(1096),
            },
            crate::PackCompression::Ordinary,
            (
                crate::ReadLimits {
                    max_object_bytes: limits.max_object_bytes,
                    max_delta_bytes: limits.max_delta_bytes,
                    ..limits.known_read
                },
                control.cancel,
            ),
            limits.max_delta_depth,
            // Like Git's fetch, and so import finds bases by position rather than identity.
            crate::pack::DeltaFraming::Offset,
            &mut || control.check().map_err(crate::PackWriteError::Io),
            &mut |done, total| {
                observe(LocalFetchProgress::Packing {
                    objects: (done, total),
                })
            },
        )
        .map_err(|error| match error {
            crate::pack::ReuseError::Read { id, source } => FetchError::LocalRead { id, source },
            crate::pack::ReuseError::Write(crate::PackWriteError::Io(error))
                if crate::transport::interruption(&error).is_some() =>
            {
                FetchError::from(error)
            }
            crate::pack::ReuseError::Write(error) => FetchError::PackWrite(error),
        })?;
        std::io::Write::flush(&mut sink)?;
        written
    };
    // Release the source packs and their caches before validation builds its own.
    drop(store);
    check(control)?;
    // Never trust source index identities for reused bytes. Import derives a new index and checks
    // delta depth, identity, framing and connectivity before any destination mutation.
    let received = ReceivedFetch::validate_known(
        advertisement,
        wants,
        super::bytes::Bytes::File(temporary, written.pack_bytes as usize),
        known,
        shallow,
        super::FetchOptions {
            limits,
            depth,
            filter: None,
        },
        &mut super::progress::ValidationObserver::new(control.cancel, &mut validation),
    )?;
    check(control)?;
    observe(LocalFetchProgress::Complete);
    Ok(received)
}

/// Selects every object reachable from `wants` with the source's bitmap index, when the
/// destination has none of them.
///
/// A clone otherwise reads and parses every commit and tree in the source to find the same set.
/// Returns `None` to walk instead: when the destination has any selected object, a depth limit or
/// shallow boundary applies, or the source has no bitmap for these wants. The selection trusts the
/// source's derived metadata only to choose objects; the destination's import still hashes every
/// received object and checks its connectivity, so a stale or wrong bitmap fails validation rather
/// than storing a gap.
fn bitmap_selection(
    store: &crate::Objects,
    wants: &[ObjectId],
    known: &KnownHistory,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<Option<Vec<ObjectId>>, FetchError> {
    if depth.is_some() || !known.shallow.is_empty() {
        return Ok(None);
    }
    let query = crate::bitmap::QueryLimits {
        max_objects: limits.max_objects,
        max_edges: limits.max_connectivity_edges,
        max_walk_bytes: limits.max_decode_bytes as u64,
        ..Default::default()
    };
    // The query observes cancellation but not the deadline, which is checked once it returns. Its
    // limits bound the work, and on git/git it takes under a second.
    let Ok(Some(reachable)) = store.bitmap_difference(wants, &[], query, control.cancel) else {
        check(control)?;
        return Ok(None);
    };
    check(control)?;
    if reachable.len() > limits.max_objects {
        return Err(FetchError::Limit("local objects"));
    }
    let ids: Vec<_> = reachable.ids().collect();
    // The walk sends every object the destination lacks and records the others as dependencies.
    // When it lacks all of them, both choose the same objects. Like the walk, send the empty tree
    // even though readers synthesize it.
    let empty_tree = store.object_format().empty_tree();
    for &id in &ids {
        check(control)?;
        if id != empty_tree && known.kind(id).is_some() {
            return Ok(None);
        }
    }
    Ok(Some(ids))
}

pub(super) fn advertise(
    source: &Repository,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<Advertisement, FetchError> {
    check(control)?;
    let store = source
        .objects_with(limits.source_packs)
        .map_err(FetchError::SourceObjects)?;
    advertise_store(source, &store, limits, control)
}

fn advertise_store(
    source: &Repository,
    store: &crate::Objects,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<Advertisement, FetchError> {
    let refs = source
        .references()
        .map_err(|_| FetchError::Protocol("local references"))?;
    let mut advertised = Vec::new();
    let mut remaining = limits.max_advertisement_bytes;
    let head = RefName::HEAD;
    let head_target = refs
        .read(&head)
        .map_err(|_| FetchError::Protocol("local HEAD"))?;
    let head_id = refs
        .resolve_with(&head, 32)
        .map_err(|_| FetchError::Protocol("local HEAD"))?
        .id;
    if let Some(id) = head_id {
        add_ref(
            &mut advertised,
            &mut remaining,
            limits.max_refs,
            head,
            id,
            false,
        )?;
    }
    for reference in refs
        .list()
        .map_err(|_| FetchError::Protocol("local references"))?
    {
        check(control)?;
        let id = match reference.target {
            Target::Direct(id) => Some(id),
            Target::Symbolic(_) => {
                refs.resolve_with(&reference.name, 32)
                    .map_err(|_| FetchError::Protocol("local symbolic reference"))?
                    .id
            }
        };
        if let Some(id) = id {
            let tag_name = reference.name.as_bytes().starts_with(b"refs/tags/");
            add_ref(
                &mut advertised,
                &mut remaining,
                limits.max_refs,
                reference.name.clone(),
                id,
                false,
            )?;
            if tag_name {
                let peeled = store
                    .peel(id, Default::default(), control.cancel)
                    .map_err(|error| FetchError::Peel(Box::new(error)))?;
                if !peeled.tags.is_empty() {
                    add_ref(
                        &mut advertised,
                        &mut remaining,
                        limits.max_refs,
                        reference.name,
                        peeled.target,
                        true,
                    )?;
                }
            }
        }
    }
    let mut capabilities = match head_target {
        Some(Target::Symbolic(name)) if name.as_bytes().starts_with(b"refs/heads/") => {
            let mut hint = b"symref=HEAD:".to_vec();
            hint.extend_from_slice(name.as_bytes());
            vec![hint]
        }
        _ => Vec::new(),
    };
    if source.object_format() == crate::ObjectFormat::Sha256 {
        capabilities.push(b"object-format=sha256".to_vec());
    }
    for capability in &capabilities {
        remaining = remaining
            .checked_sub(capability.len() + 1)
            .ok_or(FetchError::Limit("local advertisement bytes"))?;
    }
    Ok(Advertisement {
        refs: advertised,
        capabilities,
    })
}

fn add_ref(
    advertised: &mut Vec<AdvertisedRef>,
    remaining: &mut usize,
    max_refs: usize,
    name: RefName,
    id: ObjectId,
    peeled: bool,
) -> Result<(), FetchError> {
    if advertised.len() == max_refs {
        return Err(FetchError::Limit("local references"));
    }
    let bytes = name.as_bytes().len() + id.as_bytes().len() * 2 + 8;
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(FetchError::Limit("local advertisement bytes"))?;
    advertised.push(AdvertisedRef {
        name,
        id,
        kind: if peeled {
            crate::fetch::AdvertisementKind::PeeledHint
        } else {
            crate::fetch::AdvertisementKind::Tip
        },
    });
    Ok(())
}

fn check(control: TransportControl<'_>) -> Result<(), FetchError> {
    control.check().map_err(FetchError::from)
}

mod prefetch;

#[cfg(test)]
mod tests;
