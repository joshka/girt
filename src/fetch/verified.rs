//! Receive-owned evidence for structural walks, never a source of object content.
use crate::{IdMap, Object, ObjectId, ObjectKind, ReadLimits};

/// Kind, size and structure of every identity-checked received object.
///
/// Commit and tag edges stay per object for ancestry and connectivity walks. Tree entries are the
/// bulk of a history's edges (62.9M on git/git, against 409k objects), so import records only the
/// kind each tree expects of each distinct target. Connectivity then proves every received tree
/// at once from that table instead of walking tree edges again.
#[derive(Debug, Default)]
pub(super) struct VerifiedObjects {
    pub objects: IdMap<VerifiedObject>,
    trees: TreeTargets,
}

/// Entry targets named by received trees, with the kind each entry's mode requires.
#[derive(Debug, Default)]
struct TreeTargets {
    /// Targets are names from received trees, unverified until connectivity, so a peer chooses
    /// them.
    kinds: crate::PeerIdMap<ObjectKind>,
    /// Typed tree entries, including repeats, for connectivity edge budgets.
    edges: usize,
    /// A received tree failed to parse or named one target with two kinds. Only a walk that
    /// reaches that tree can decide the outcome.
    unproven: bool,
}

/// Received trees whose entries all parsed with consistent target kinds.
pub(super) struct TreeProof<'a> {
    /// Each distinct target with the kind every naming tree requires.
    pub targets: &'a crate::PeerIdMap<ObjectKind>,
    /// Typed entries across all received trees.
    pub edges: usize,
}

impl VerifiedObjects {
    /// Records an identity-checked object's kind, size and structure.
    pub fn insert(&mut self, id: ObjectId, object: &Object) {
        self.record(id, Described::new(id, object));
    }

    /// Records a description computed by [`Described::new`], possibly on another thread.
    pub fn record(&mut self, id: ObjectId, described: Described) {
        let Described {
            kind,
            bytes,
            parsed,
        } = described;
        let structure = match parsed {
            Parsed::Edges(edges) => Structure::Edges(edges),
            Parsed::Tree(entries) => {
                self.trees.edges += entries.len();
                for (target, kind) in entries {
                    if *self.trees.kinds.entry(target).or_insert(kind) != kind {
                        self.trees.unproven = true;
                    }
                }
                Structure::Tree
            }
            Parsed::Malformed => {
                if kind == ObjectKind::Tree {
                    self.trees.unproven = true;
                }
                Structure::Malformed
            }
        };
        self.objects.insert(
            id,
            VerifiedObject {
                kind,
                bytes,
                structure,
                read: ReadLimits {
                    max_object_bytes: bytes,
                    max_delta_bytes: 0,
                    max_delta_depth: 0,
                    max_decode_bytes: bytes,
                    max_input_bytes: 0,
                },
            },
        );
    }

    /// Distinct entry targets named by received trees.
    pub fn tree_targets(&self) -> usize {
        self.trees.kinds.len()
    }

    /// Received objects, their payload bytes and their typed edges, for walk budgets.
    pub fn totals(&self) -> (usize, usize, usize) {
        let bytes = self.objects.values().map(|object| object.bytes).sum();
        let edges = self
            .objects
            .values()
            .filter_map(VerifiedObject::edges)
            .map(<[_]>::len)
            .sum::<usize>();
        (self.objects.len(), bytes, edges + self.trees.edges)
    }

    /// The received trees' entry targets, when every received tree parsed consistently.
    pub fn tree_proof(&self) -> Option<TreeProof<'_>> {
        (!self.trees.unproven).then_some(TreeProof {
            targets: &self.trees.kinds,
            edges: self.trees.edges,
        })
    }
}

/// An identity-checked object's kind, size and parsed edges, before it joins [`VerifiedObjects`].
pub(super) struct Described {
    kind: ObjectKind,
    bytes: usize,
    parsed: Parsed,
}

enum Parsed {
    Edges(Vec<(ObjectId, ObjectKind)>),
    Tree(Vec<(ObjectId, ObjectKind)>),
    Malformed,
}

impl Described {
    /// Parses `object`'s typed edges; needs no shared state, so it can run on any thread.
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
            parsed: match parsed {
                Err(_) => Parsed::Malformed,
                Ok(()) if object.kind() == ObjectKind::Tree => Parsed::Tree(edges),
                Ok(()) => Parsed::Edges(edges),
            },
        }
    }
}

#[cfg(test)]
impl VerifiedObjects {
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    /// Typed edges kept after import for objects of `kind`.
    pub fn retained_edges(&self, kind: ObjectKind) -> usize {
        self.objects
            .values()
            .filter(|object| object.kind == kind)
            .filter_map(VerifiedObject::edges)
            .map(<[_]>::len)
            .sum()
    }
}

#[derive(Debug)]
pub(super) struct VerifiedObject {
    pub kind: ObjectKind,
    pub bytes: usize,
    structure: Structure,
    pub read: ReadLimits,
}

#[derive(Debug)]
enum Structure {
    /// Typed edges of a commit or tag; blobs have none.
    Edges(Vec<(ObjectId, ObjectKind)>),
    /// A tree whose entries [`VerifiedObjects::tree_proof`] covers.
    Tree,
    /// Unselected malformed server extras need not parse during import.
    Malformed,
}

impl VerifiedObject {
    /// Typed edges of a received commit, tag or blob. Trees are proven collectively instead.
    pub fn edges(&self) -> Option<&[(ObjectId, ObjectKind)]> {
        match &self.structure {
            Structure::Edges(edges) => Some(edges),
            Structure::Tree | Structure::Malformed => None,
        }
    }

    /// Whether this is a received tree covered by [`VerifiedObjects::tree_proof`].
    pub fn is_proven_tree(&self) -> bool {
        matches!(self.structure, Structure::Tree)
    }

    /// Certify the uncached representation's bounds. Smaller limits use the ordinary reader,
    /// which preserves its delta-base cache behavior and exact errors.
    pub fn fits(&self, limits: ReadLimits) -> bool {
        self.edges().is_some() && self.within(limits)
    }

    /// Whether reading the uncached representation stays within `limits`.
    pub fn within(&self, limits: ReadLimits) -> bool {
        self.read.max_object_bytes <= limits.max_object_bytes
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

    /// Whether normal lookup selects the validated pack for every received object.
    ///
    /// Then every received object reads as the bytes import identity-checked, so import's
    /// connectivity proof holds for the installed graph. Objects whose recorded representation
    /// costs exceed `read` need the ordinary reader's exact limit checks, so they also answer
    /// `false`.
    pub fn selects_every_received(
        &self,
        read: ReadLimits,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<bool, crate::ObjectReadError> {
        let Some(pack) = self.pack else {
            return Ok(false);
        };
        for (&id, object) in &self.verified.objects {
            if !object.within(read) || !self.store.selects_received(id, pack, cancel)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn verified(&self) -> &'a VerifiedObjects {
        self.verified
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
