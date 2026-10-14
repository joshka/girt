use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;

use super::known::KnownObjects;
use super::verified::TreeProof;
use super::{FetchError, FetchLimits, check_cancelled};
use crate::{IdMap, IdSet, Object, ObjectId, ObjectKind};

/// Typed edges of one object, borrowed from receive evidence where it is retained.
type Edges<'a> = Cow<'a, [(ObjectId, ObjectKind)]>;

pub(super) trait ReceivedObjects {
    #[cfg(feature = "tracing")]
    fn len(&self) -> usize;
    fn contains_key(&self, id: &ObjectId) -> bool;
    fn get(
        &self,
        id: &ObjectId,
        cancel: &AtomicBool,
    ) -> Result<Option<Cow<'_, Object>>, FetchError>;
    fn edges(&self, id: ObjectId, cancel: &AtomicBool) -> Result<Option<Edges<'_>>, FetchError> {
        self.get(&id, cancel)?
            .map(|object| object_edges(id, &object).map(Cow::Owned))
            .transpose()
    }
    fn kind(&self, id: ObjectId, cancel: &AtomicBool) -> Result<Option<ObjectKind>, FetchError> {
        Ok(self.get(&id, cancel)?.map(|o| o.kind()))
    }
    /// Entry targets of every received tree, when import proved them all consistent.
    fn tree_proof(&self) -> Option<TreeProof<'_>> {
        None
    }
    /// Whether `id` is a received tree covered by [`Self::tree_proof`].
    fn proven_tree(&self, _id: ObjectId) -> bool {
        false
    }
}

impl ReceivedObjects for IdMap<Object> {
    #[cfg(feature = "tracing")]
    fn len(&self) -> usize {
        self.len()
    }
    fn contains_key(&self, id: &ObjectId) -> bool {
        self.contains_key(id)
    }
    fn get(&self, id: &ObjectId, _: &AtomicBool) -> Result<Option<Cow<'_, Object>>, FetchError> {
        Ok(self.get(id).map(Cow::Borrowed))
    }
}

impl ReceivedObjects for super::import::IndexedObjects {
    #[cfg(feature = "tracing")]
    fn len(&self) -> usize {
        self.len()
    }
    fn contains_key(&self, id: &ObjectId) -> bool {
        self.contains_key(id)
    }
    fn get(
        &self,
        id: &ObjectId,
        cancel: &AtomicBool,
    ) -> Result<Option<Cow<'_, Object>>, FetchError> {
        Ok(self.get(id, cancel)?.map(Cow::Owned))
    }
    fn edges(&self, id: ObjectId, cancel: &AtomicBool) -> Result<Option<Edges<'_>>, FetchError> {
        if let Some(edges) = self
            .verified
            .objects
            .get(&id)
            .and_then(super::verified::VerifiedObject::edges)
        {
            return Ok(Some(Cow::Borrowed(edges)));
        }
        // Trees are not retained; an exact walk re-reads them from the received pack.
        self.get(&id, cancel)?
            .map(|object| object_edges(id, &object).map(Cow::Owned))
            .transpose()
    }
    fn kind(&self, id: ObjectId, _: &AtomicBool) -> Result<Option<ObjectKind>, FetchError> {
        Ok(self.kind(id))
    }
    fn tree_proof(&self) -> Option<TreeProof<'_>> {
        self.verified.tree_proof()
    }
    fn proven_tree(&self, id: ObjectId) -> bool {
        self.verified
            .objects
            .get(&id)
            .is_some_and(super::verified::VerifiedObject::is_proven_tree)
    }
}

/// Follow typed edges from every selected tip. Gitlinks refer to independent submodule stores.
#[cfg(test)]
pub(super) fn validate(
    objects: &IdMap<Object>,
    wants: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    validate_with_known(objects, &IdMap::default(), wants, limits, cancel).map(|_| ())
}

#[cfg(test)]
pub(super) fn validate_with_known(
    objects: &IdMap<Object>,
    known: &IdMap<Object>,
    wants: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<Vec<ObjectId>, FetchError> {
    validate_with_boundaries(objects, known, wants, &[], limits, cancel)
}

pub(super) fn validate_with_boundaries(
    objects: &impl ReceivedObjects,
    known: &(impl KnownObjects + ?Sized),
    wants: &[ObjectId],
    shallow: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<Vec<ObjectId>, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.connectivity",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        received_objects = objects.len(),
        wants = wants.len(),
        visited = tracing::field::Empty,
        edges = tracing::field::Empty,
    );

    let operation = || {
        // Proving received trees collectively avoids walking their entries. Any failure there
        // defers to the exact walk, which alone decides rejections and their errors.
        if let Some(proof) = objects.tree_proof() {
            match walk(objects, known, wants, shallow, limits, cancel, Some(proof)) {
                Err(FetchError::Cancelled) => return Err(FetchError::Cancelled),
                Err(_) => {}
                Ok(walked) => return Ok(walked),
            }
        }
        walk(objects, known, wants, shallow, limits, cancel, None)
    };
    #[cfg(feature = "tracing")]
    let operation = || {
        let result = operation();
        if let Ok(walked) = &result {
            span.record("visited", walked.visited)
                .record("edges", limits.max_connectivity_edges - walked.remaining);
        }
        result.map(|walked| walked.dependencies)
    };
    #[cfg(not(feature = "tracing"))]
    let operation = || operation().map(|walked| walked.dependencies);
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

/// Dependencies on known objects, with walk statistics for tracing.
struct Walked {
    dependencies: Vec<ObjectId>,
    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    visited: usize,
    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    remaining: usize,
}

/// Follows typed edges from `wants`, stopping at trusted complete known objects.
///
/// With a tree proof, every received tree is complete as soon as each target its entries name is
/// a received object of the required kind or a known object whose history is checked here. A
/// received tree reached from a commit or tag therefore needs no expansion. Known targets of
/// unreachable received trees also become dependencies, so the result can exceed the exact walk's.
fn walk(
    objects: &impl ReceivedObjects,
    known: &(impl KnownObjects + ?Sized),
    wants: &[ObjectId],
    shallow: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
    proof: Option<TreeProof<'_>>,
) -> Result<Walked, FetchError> {
    let lookup = |id: ObjectId| -> Result<Option<Cow<'_, Object>>, FetchError> {
        Ok(objects.get(&id, cancel)?.or_else(|| known.get(id)))
    };
    let kind = |id: ObjectId| -> Result<Option<ObjectKind>, FetchError> {
        Ok(objects.kind(id, cancel)?.or_else(|| known.known_kind(id)))
    };
    let shallow: IdSet = shallow.iter().copied().collect();
    for &id in &shallow {
        let object = lookup(id)?.ok_or(FetchError::Missing(id))?;
        if object.kind() != crate::ObjectKind::Commit {
            return Err(FetchError::Kind(id));
        }
    }
    let mut dependencies = Vec::new();
    let mut pending = VecDeque::new();
    // Each visited object's actual kind, so every edge to it is checked, not just the first.
    let mut seen = IdMap::<ObjectKind>::default();
    for &id in wants {
        if seen.contains_key(&id) {
            continue;
        }
        let actual = kind(id)?.ok_or(FetchError::Missing(id))?;
        seen.insert(id, actual);
        pending.push_back(id);
    }
    let mut remaining = limits.max_connectivity_edges;
    if let Some(proof) = &proof {
        remaining = remaining
            .checked_sub(proof.edges)
            .ok_or(FetchError::Limit("connectivity edges"))?;
        for (&target, &expected) in proof.targets {
            check_cancelled(cancel)?;
            let actual = kind(target)?.ok_or(FetchError::Missing(target))?;
            if actual != expected {
                return Err(FetchError::Kind(target));
            }
            // Received targets are blobs or proven trees. Known ones are walked like any other
            // dependency.
            if seen.insert(target, actual).is_none() && !objects.contains_key(&target) {
                pending.push_back(target);
            }
        }
    }
    while let Some(id) = pending.pop_front() {
        check_cancelled(cancel)?;
        if !objects.contains_key(&id) {
            dependencies.push(id);
            if known.trusted_complete(id) {
                // Only explicit complete boundaries stop the walk, never arbitrary stored
                // objects.
                continue;
            }
        }
        // Blob identity and kind were validated while indexing; blobs have no outgoing edges.
        if kind(id)? == Some(ObjectKind::Blob) {
            continue;
        }
        if proof.is_some() && objects.proven_tree(id) {
            continue;
        }
        let edges = match objects.edges(id, cancel)? {
            Some(edges) => edges,
            None => {
                let object = lookup(id)?.ok_or(FetchError::Missing(id))?;
                Cow::Owned(object_edges(id, &object)?)
            }
        };
        let shallow_commit = shallow.contains(&id) && kind(id)? == Some(ObjectKind::Commit);
        let mut edge = |id, expected| {
            if shallow_commit && expected == crate::ObjectKind::Commit {
                return Ok(());
            }
            check_cancelled(cancel)?;
            remaining = remaining
                .checked_sub(1)
                .ok_or(FetchError::Limit("connectivity edges"))?;
            let actual = match seen.get(&id) {
                Some(&actual) => actual,
                None => {
                    let actual = kind(id)?.ok_or(FetchError::Missing(id))?;
                    seen.insert(id, actual);
                    pending.push_back(id);
                    actual
                }
            };
            if actual != expected {
                return Err(FetchError::Kind(id));
            }
            Ok(())
        };
        for &(target, expected) in edges.iter() {
            edge(target, expected)?;
        }
    }
    Ok(Walked {
        visited: seen.len(),
        dependencies,
        remaining,
    })
}

fn object_edges(id: ObjectId, object: &Object) -> Result<Vec<(ObjectId, ObjectKind)>, FetchError> {
    let mut edges = Vec::new();
    crate::edges::visit(id, object.kind(), object.data(), |id, kind| {
        edges.push((id, kind));
        Ok::<_, FetchError>(())
    })?;
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        Commit, CommitFields, EntryMode, ObjectKind, Signature, Tag, TagFields, Tree, TreeEntry,
    };

    fn object(kind: ObjectKind, data: Vec<u8>) -> Object {
        Object {
            kind,
            data: data.into(),
            format: crate::ObjectFormat::Sha1,
        }
    }
    fn validate_root(
        root: Object,
        children: Vec<Object>,
        limits: FetchLimits,
    ) -> Result<(), FetchError> {
        let id = root.id();
        let objects = std::iter::once(root)
            .chain(children)
            .map(|o| (o.id(), o))
            .collect();
        validate(&objects, &[id], limits, &AtomicBool::new(false))
    }
    fn commit(tree: ObjectId, parents: Vec<ObjectId>) -> Object {
        let author = Signature {
            name: b"A".to_vec(),
            email: b"a@example.com".to_vec(),
            seconds: 0,
            offset_minutes: 0,
        };
        let data = Commit::new(CommitFields {
            tree,
            parents,
            author: author.clone(),
            committer: author,
            extra_headers: vec![],
            message: vec![],
        })
        .unwrap()
        .encode();
        object(ObjectKind::Commit, data)
    }
    fn tree(id: ObjectId, mode: EntryMode) -> Object {
        object(
            ObjectKind::Tree,
            Tree::new(
                crate::ObjectFormat::Sha1,
                vec![TreeEntry {
                    id,
                    mode,
                    name: b"entry".to_vec(),
                }],
            )
            .unwrap()
            .encode(),
        )
    }
    fn tag(target: ObjectId, kind: ObjectKind) -> Object {
        object(
            ObjectKind::Tag,
            Tag::new(TagFields {
                target,
                target_kind: kind,
                name: b"tag".to_vec(),
                tagger: None,
                extra_headers: vec![],
                message: vec![],
            })
            .unwrap()
            .encode(),
        )
    }
    #[rstest]
    #[case::missing_tree(commit(ObjectId::for_blob(crate::ObjectFormat::Sha1, b"missing"), vec![]))]
    #[case::missing_blob(tree(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"missing"),
        EntryMode::Blob
    ))]
    #[case::missing_tag_target(tag(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"missing"),
        ObjectKind::Blob
    ))]
    fn rejects_missing_edges(#[case] root: Object) {
        assert!(matches!(
            validate_root(root, vec![], FetchLimits::default()),
            Err(FetchError::Missing(_))
        ));
    }
    #[test]
    fn rejects_missing_parent_even_when_tree_is_complete() {
        let tree = object(ObjectKind::Tree, vec![]);
        let root = commit(
            tree.id(),
            vec![ObjectId::for_blob(crate::ObjectFormat::Sha1, b"missing")],
        );
        assert!(matches!(
            validate_root(root, vec![tree], FetchLimits::default()),
            Err(FetchError::Missing(_))
        ));
    }
    #[rstest]
    #[case::commit_tree(commit(ObjectId::for_blob(crate::ObjectFormat::Sha1, b"payload"), vec![]))]
    #[case::subtree(tree(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"payload"),
        EntryMode::Tree
    ))]
    #[case::tag_target(tag(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"payload"),
        ObjectKind::Commit
    ))]
    fn rejects_wrong_edge_kind(#[case] root: Object) {
        assert!(matches!(
            validate_root(
                root,
                vec![object(ObjectKind::Blob, b"payload".to_vec())],
                FetchLimits::default()
            ),
            Err(FetchError::Kind(_))
        ));
    }
    #[test]
    fn skips_gitlinks_to_external_submodules() {
        let root = tree(
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"external"),
            EntryMode::Gitlink,
        );
        assert!(validate_root(root, vec![], FetchLimits::default()).is_ok());
    }
    #[test]
    fn bounds_edge_occurrences() {
        let child = object(ObjectKind::Blob, b"data".to_vec());
        let root = tree(child.id(), EntryMode::Blob);
        assert!(matches!(
            validate_root(
                root,
                vec![child],
                FetchLimits {
                    max_connectivity_edges: 0,
                    ..FetchLimits::default()
                }
            ),
            Err(FetchError::Limit("connectivity edges"))
        ));
    }
    #[test]
    fn nested_tags_can_reach_noncommit_objects() {
        let child = object(ObjectKind::Blob, b"data".to_vec());
        let inner = tag(child.id(), ObjectKind::Blob);
        let root = tag(inner.id(), ObjectKind::Tag);
        assert!(validate_root(root, vec![inner, child], FetchLimits::default()).is_ok());
    }
    #[rstest]
    #[case::tree(ObjectKind::Tree)]
    #[case::commit(ObjectKind::Commit)]
    #[case::tag(ObjectKind::Tag)]
    fn rejects_malformed_reachable_payload(#[case] kind: ObjectKind) {
        let root = object(kind, b"invalid".to_vec());
        let id = root.id();
        let error = validate_root(root, vec![], FetchLimits::default()).unwrap_err();
        assert!(error.to_string().contains(&id.to_string()));
        assert!(std::error::Error::source(&error).is_some());
    }
    #[test]
    fn combined_graph_records_only_used_local_dependencies() {
        let blob = object(ObjectKind::Blob, b"known".to_vec());
        let unrelated = object(ObjectKind::Blob, b"unrelated".to_vec());
        let root = tree(blob.id(), EntryMode::Blob);
        let id = root.id();
        let expected = blob.id();
        let received = crate::IdMap::from_iter([(id, root)]);
        let known = crate::IdMap::from_iter([(blob.id(), blob), (unrelated.id(), unrelated)]);
        let dependencies = validate_with_known(
            &received,
            &known,
            &[id],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(dependencies, vec![expected]);
    }

    #[test]
    fn combined_graph_rejects_wrong_local_edge_kind() {
        let blob = object(ObjectKind::Blob, b"known".to_vec());
        let root = tree(blob.id(), EntryMode::Tree);
        let id = root.id();
        let received = crate::IdMap::from_iter([(id, root)]);
        let known = crate::IdMap::from_iter([(blob.id(), blob)]);
        assert!(matches!(
            validate_with_known(
                &received,
                &known,
                &[id],
                FetchLimits::default(),
                &AtomicBool::new(false)
            ),
            Err(FetchError::Kind(_))
        ));
    }

    #[test]
    fn combined_graph_charges_local_edges_to_verification_budget() {
        let blob = object(ObjectKind::Blob, b"known".to_vec());
        let root = tree(blob.id(), EntryMode::Blob);
        let id = root.id();
        let known = crate::IdMap::from_iter([(id, root), (blob.id(), blob)]);
        assert!(matches!(
            validate_with_known(
                &IdMap::default(),
                &known,
                &[id],
                FetchLimits {
                    max_connectivity_edges: 0,
                    ..FetchLimits::default()
                },
                &AtomicBool::new(false)
            ),
            Err(FetchError::Limit("connectivity edges"))
        ));
    }
}
