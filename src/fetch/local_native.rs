//! Native local advertisement and complete reachable-pack construction.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;

use super::{
    AdvertisedRef, Advertisement, FetchError, FetchLimits, KnownHistory, LocalFetchProgress,
    NativeContents, ReceivedFetch,
};
use crate::refs::{RefName, Target};
use crate::transport::TransportControl;
use crate::{Object, ObjectId, ObjectKind, PackObject, PackWriteLimits, Repository};

pub(super) fn receive(
    source: &Repository,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    mut progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    mut observe: impl FnMut(LocalFetchProgress),
) -> Result<ReceivedFetch, FetchError> {
    check(control)?;
    if !source.shallow_roots().is_empty() {
        return Err(FetchError::Unsupported("shallow local source"));
    }
    let advertisement = advertise(source, limits, control)?;
    let store = source
        .objects(Default::default())
        .map_err(FetchError::Destination)?;
    let wants = select(&advertisement);
    if wants.len() > limits.max_wants {
        return Err(FetchError::Limit("wants"));
    }
    let offered: HashSet<_> = advertisement
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
                pack: Vec::new(),
                index: Vec::new(),
                checksum: None,
                objects: 0,
                dependencies: Vec::new(),
                limits,
            },
        ));
    }
    let mut pending: VecDeque<_> = wants.iter().copied().map(|id| (id, None)).collect();
    let mut expected = HashMap::<ObjectId, Option<ObjectKind>>::new();
    let mut observed = HashMap::<ObjectId, ObjectKind>::new();
    let mut selected = Vec::<(ObjectId, Object)>::new();
    let mut dependencies = Vec::new();
    let mut bytes = 0_usize;
    let mut edges = 0_usize;
    while let Some((id, kind)) = pending.pop_front() {
        check(control)?;
        if let Some(previous) = expected.get_mut(&id) {
            if kind.is_some() && previous.is_some() && *previous != kind {
                return Err(FetchError::Kind(id));
            }
            if kind.is_some_and(|kind| observed[&id] != kind) {
                return Err(FetchError::Kind(id));
            }
            if kind.is_some() {
                *previous = kind;
            }
            continue;
        }
        if expected.len() >= limits.max_objects {
            return Err(FetchError::Limit("local objects"));
        }
        expected.insert(id, kind);
        let mut read = limits.known_read;
        read.max_object_bytes = read.max_object_bytes.min(limits.max_object_bytes);
        let object = store
            .read(id, read)
            .map_err(|source| FetchError::LocalRead { id, source })?
            .or_else(|| {
                // Git can name the canonical empty tree without storing its loose or packed
                // object. Its bytes and identity are fixed in either object format.
                let empty = Object {
                    kind: ObjectKind::Tree,
                    format: id.format(),
                    data: Vec::new(),
                };
                (id == empty.id()).then_some(empty)
            })
            .ok_or(FetchError::Missing(id))?;
        if kind.is_some_and(|kind| kind != object.kind()) {
            return Err(FetchError::Kind(id));
        }
        observed.insert(id, object.kind());
        bytes = bytes
            .checked_add(object.data().len())
            .ok_or(FetchError::Limit("local bytes"))?;
        if bytes > limits.max_decode_bytes {
            return Err(FetchError::Limit("local bytes"));
        }
        crate::edges::visit(id, &object, |target, kind| {
            check(control)?;
            edges += 1;
            if edges > limits.max_connectivity_edges {
                return Err(FetchError::Limit("local edges"));
            }
            pending.push_back((target, Some(kind)));
            Ok(())
        })?;
        if limits.max_haves > 0 && known.objects.get(&id).is_some_and(|known| known == &object) {
            dependencies.push(id);
        } else {
            selected.push((id, object));
        }
        observe(LocalFetchProgress::Reading {
            objects: observed.len() as u64,
        });
        check(control)?;
    }
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
                pack: Vec::new(),
                index: Vec::new(),
                checksum: None,
                objects: 0,
                dependencies,
                limits,
            },
        ));
    }
    let inputs: Vec<_> = selected
        .iter()
        .map(|(id, object)| PackObject {
            id: *id,
            kind: object.kind(),
            data: object.data(),
        })
        .collect();
    let mut pack = Vec::new();
    let mut index = Vec::new();
    let written = crate::pack::write_controlled_observed(
        source.object_format(),
        &inputs,
        (&mut pack, &mut index),
        PackWriteLimits {
            max_objects: limits.max_objects.try_into().unwrap_or(u32::MAX),
            max_object_bytes: limits.max_object_bytes as u64,
            max_input_bytes: limits.max_decode_bytes as u64,
            max_pack_bytes: limits.max_pack_bytes as u64,
            ..Default::default()
        },
        crate::PackCompression::Ordinary,
        &mut || control.check().map_err(crate::PackWriteError::Io),
        &mut |done, total| {
            observe(LocalFetchProgress::Packing {
                objects: (done, total),
            })
        },
    );
    let written = written.map_err(|error| match error {
        crate::PackWriteError::Io(error) if crate::transport::interruption(&error).is_some() => {
            FetchError::from(error)
        }
        error => FetchError::PackWrite(error),
    })?;
    check(control)?;
    observe(LocalFetchProgress::Complete);
    Ok(ReceivedFetch::native(
        advertisement,
        wants,
        NativeContents {
            format: source.object_format(),
            pack,
            index,
            checksum: Some(written.checksum),
            objects: selected.len(),
            dependencies,
            limits,
        },
    ))
}

pub(super) fn advertise(
    source: &Repository,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<Advertisement, FetchError> {
    check(control)?;
    let store = source
        .objects(Default::default())
        .map_err(FetchError::Destination)?;
    let refs = source
        .references()
        .map_err(|_| FetchError::Protocol("local references"))?;
    let mut advertised = Vec::new();
    let mut remaining = limits.max_advertisement_bytes;
    let head = RefName::new(b"HEAD").expect("valid HEAD");
    let head_target = refs
        .read(&head)
        .map_err(|_| FetchError::Protocol("local HEAD"))?;
    let head_id = refs
        .resolve(&head, 32)
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
                refs.resolve(&reference.name, 32)
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
    advertised.push(AdvertisedRef { name, id, peeled });
    Ok(())
}

fn check(control: TransportControl<'_>) -> Result<(), FetchError> {
    control.check().map_err(FetchError::from)
}

#[cfg(test)]
mod tests;
