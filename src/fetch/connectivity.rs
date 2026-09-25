use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchLimits, check_cancelled};
use crate::{Object, ObjectId};

/// Follow typed edges from every selected tip. Gitlinks refer to independent submodule stores.
#[cfg(test)]
pub(super) fn validate(
    objects: &HashMap<ObjectId, Object>,
    wants: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    validate_with_known(objects, &HashMap::new(), wants, limits, cancel).map(|_| ())
}

pub(super) fn validate_with_known(
    objects: &HashMap<ObjectId, Object>,
    known: &HashMap<ObjectId, Object>,
    wants: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<Vec<ObjectId>, FetchError> {
    let mut dependencies = Vec::new();
    let mut pending = VecDeque::new();
    let mut seen = HashSet::new();
    for &id in wants {
        if !(objects.contains_key(&id) || known.contains_key(&id)) {
            return Err(FetchError::Missing(id));
        }
        if seen.insert(id) {
            pending.push_back(id);
        }
    }
    let mut remaining = limits.max_connectivity_edges;
    while let Some(id) = pending.pop_front() {
        check_cancelled(cancel)?;
        let object = objects.get(&id).or_else(|| known.get(&id)).unwrap();
        if !objects.contains_key(&id) {
            dependencies.push(id);
        }
        let edge = |id, kind| {
            check_cancelled(cancel)?;
            remaining = remaining
                .checked_sub(1)
                .ok_or(FetchError::Limit("connectivity edges"))?;
            let object = objects
                .get(&id)
                .or_else(|| known.get(&id))
                .ok_or(FetchError::Missing(id))?;
            if object.kind() != kind {
                return Err(FetchError::Kind(id));
            }
            if seen.insert(id) {
                pending.push_back(id);
            }
            Ok(())
        };
        crate::edges::visit(id, object, edge)?;
    }
    Ok(dependencies)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        Commit, CommitFields, EntryMode, ObjectKind, Signature, Tag, TagFields, Tree, TreeEntry,
    };

    fn object(kind: ObjectKind, data: Vec<u8>) -> Object {
        Object { kind, data }
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
            Tree::new(vec![TreeEntry {
                id,
                mode,
                name: b"entry".to_vec(),
            }])
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
        let received = HashMap::from([(id, root)]);
        let known = HashMap::from([(blob.id(), blob), (unrelated.id(), unrelated)]);
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
        let received = HashMap::from([(id, root)]);
        let known = HashMap::from([(blob.id(), blob)]);
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
        let known = HashMap::from([(id, root), (blob.id(), blob)]);
        assert!(matches!(
            validate_with_known(
                &HashMap::new(),
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
