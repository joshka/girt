//! Typed edges in supported Git payloads; traversal and budgets belong to callers.

use crate::{Commit, EntryMode, ObjectId, ObjectKind, Tag};

/// Visits edges in payload order, preserving duplicates and skipping external gitlinks.
/// Parsing validates readable commit/tag syntax and tree entry constraints, without
/// canonicalizing the payload or imposing construction-only signature rules.
///
/// `data` is the payload of `id`, in `id`'s format, read as `kind`.
pub(crate) fn visit<E: From<Error>>(
    id: ObjectId,
    kind: ObjectKind,
    data: &[u8],
    mut edge: impl FnMut(ObjectId, ObjectKind) -> Result<(), E>,
) -> Result<(), E> {
    let format = id.format();
    match kind {
        ObjectKind::Blob => {}
        ObjectKind::Commit => {
            let commit =
                Commit::parse(format, data).map_err(|source| Error::Commit { id, source })?;
            edge(commit.tree(), ObjectKind::Tree)?;
            for &parent in commit.parents() {
                edge(parent, ObjectKind::Commit)?;
            }
        }
        ObjectKind::Tree => {
            // Check the whole payload first so a malformed tree reports no edges, as before.
            let entries = || crate::tree::RawEntries::new(format, data);
            if let Some(Err(source)) = entries().find(Result::is_err) {
                return Err(Error::Tree { id, source }.into());
            }
            for (mode, _, target) in entries().flatten() {
                match mode {
                    EntryMode::Gitlink => {}
                    EntryMode::Tree => edge(target, ObjectKind::Tree)?,
                    _ => edge(target, ObjectKind::Blob)?,
                }
            }
        }
        ObjectKind::Tag => {
            let tag = Tag::parse(format, data).map_err(|source| Error::Tag { id, source })?;
            edge(tag.target(), tag.target_kind())?;
        }
    }
    Ok(())
}

/// Private parse failures translated to each workflow's public diagnostic.
pub(crate) enum Error {
    Commit {
        id: ObjectId,
        source: crate::CommitError,
    },
    Tree {
        id: ObjectId,
        source: crate::TreeError,
    },
    Tag {
        id: ObjectId,
        source: crate::TagError,
    },
}
