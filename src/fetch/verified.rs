//! Receive-owned evidence for structural walks, never a source of object content.
use std::collections::HashMap;

use crate::{Object, ObjectId, ObjectKind, ReadLimits};

#[derive(Debug, Default)]
pub(super) struct VerifiedObjects {
    pub objects: HashMap<ObjectId, VerifiedObject>,
}

#[derive(Debug)]
pub(super) struct VerifiedObject {
    pub kind: ObjectKind,
    pub bytes: usize,
    /// Unselected malformed server extras need not be parsed successfully during import.
    pub edges: Option<Vec<(ObjectId, ObjectKind)>>,
    pub read: ReadLimits,
}

impl VerifiedObject {
    pub fn new(id: ObjectId, object: &Object) -> Self {
        let mut edges = Vec::new();
        let parsed = crate::edges::visit::<super::FetchError>(
            id,
            object.kind(),
            object.data(),
            |id, kind| {
                edges.push((id, kind));
                Ok(())
            },
        );
        Self {
            kind: object.kind(),
            bytes: object.data().len(),
            edges: parsed.ok().map(|()| edges),
            read: ReadLimits {
                max_object_bytes: object.data().len(),
                max_delta_bytes: 0,
                max_delta_depth: 0,
                max_decode_bytes: object.data().len(),
                max_input_bytes: 0,
            },
        }
    }

    /// Certify the uncached representation's bounds. Smaller limits use the ordinary reader,
    /// which preserves its delta-base cache behavior and exact errors.
    pub fn fits(&self, limits: ReadLimits) -> bool {
        self.edges.is_some()
            && self.read.max_object_bytes <= limits.max_object_bytes
            && self.read.max_delta_bytes <= limits.max_delta_bytes
            && self.read.max_delta_depth <= limits.max_delta_depth
            && self.read.max_decode_bytes <= limits.max_decode_bytes
            && self.read.max_input_bytes <= limits.max_input_bytes
    }
}

/// Proof lives only for one pinned destination snapshot and one receive result. Nothing here
/// returns payloads or grants trust to a different representation of the same identity.
pub(super) struct InstalledObjects<'a> {
    store: &'a crate::Objects,
    verified: &'a VerifiedObjects,
    pack: Option<&'a crate::pack::FilePack>,
}

impl<'a> InstalledObjects<'a> {
    pub fn store(&self) -> &'a crate::Objects {
        self.store
    }

    pub fn bind(
        store: &'a crate::Objects,
        received: &'a super::ReceivedFetch,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, super::FetchError> {
        let mut selected = None;
        if !received.verified.objects.is_empty() {
            for pack in store.transfer_packs() {
                if pack
                    .matches_received(
                        &received.index,
                        received.pack.reader()?,
                        received.pack.len(),
                        cancel,
                    )
                    .map_err(super::FetchError::Destination)?
                {
                    selected = Some(pack);
                    break;
                }
            }
        }
        Ok(Self {
            store,
            verified: &received.verified,
            pack: selected,
        })
    }

    pub fn get(
        &self,
        id: ObjectId,
        read: ReadLimits,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<&VerifiedObject>, crate::ObjectReadError> {
        let Some(object) = self.verified.objects.get(&id).filter(|o| o.fits(read)) else {
            return Ok(None);
        };
        let Some(pack) = self.pack else {
            return Ok(None);
        };
        let selected = self.store.selects_received(id, pack, cancel)?;
        Ok(selected.then_some(object))
    }
}
