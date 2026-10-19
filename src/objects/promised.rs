//! Objects a partial clone's promisor remote has promised to supply.
//!
//! A partial clone fetches with an object filter, so some trees and blobs its commits name are
//! never stored locally. Fetch marks each pack it received from a promisor remote with a
//! `.promisor` file. Git treats an object as promised when an object in such a pack references
//! it: the remote sent the referencing object and can supply its dependencies later. A missing
//! object that nothing in a promisor pack references is damage, not a promise.
//!
//! Reading never fetches. A missing object reads as absent, and callers that want it decide
//! whether and when to fetch it.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

use super::Objects;
use crate::pack::reader::check_cancelled;
use crate::{ListLimits, ObjectId, ObjectKind, ObjectReadError, ReadLimits};

/// Identities referenced by objects in promisor packs, built by [`Objects::promised_objects`].
///
/// This is a snapshot of the packs pinned by the reader that built it. Objects stored in a
/// promisor pack are not included unless another promisor-pack object references them; ask
/// [`IndexedPack::is_promisor`](crate::IndexedPack::is_promisor) about stored objects.
#[derive(Clone, Debug, Default)]
pub struct PromisedObjects {
    ids: HashSet<ObjectId>,
}

impl PromisedObjects {
    /// Whether some object in a promisor pack references `id`.
    ///
    /// For an object that is not stored, `true` means the promisor remote can supply it and its
    /// absence is expected; `false` means it is missing without a promise.
    pub fn contains(&self, id: ObjectId) -> bool {
        self.ids.contains(&id)
    }

    /// Number of distinct referenced identities, stored or not.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no promisor-pack object references anything, as in a repository without
    /// promisor packs.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Objects an operation needs that this partial clone does not store yet.
///
/// A partial clone's promisor remotes can supply objects it left out. Operations that read many
/// objects, such as checkout, patch rendering and rename detection, collect every needed object
/// they find missing before failing, so one batch fetch can supply them all. Blame reads history
/// one version at a time and reports the first missing blob. In other repositories a missing
/// object is damage, and each operation reports it with its own error instead.
///
/// Object reads never fetch. Pass [`Self::ids`] to
/// [`workflow::fetch_promised`](crate::workflow::fetch_promised), or use
/// [`workflow::fetch_needed`](crate::workflow::fetch_needed), then run the operation again.
///
/// # Examples
///
/// ```
/// use girt::{MissingObjectsError, ObjectFormat, ObjectKind};
///
/// let blob = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"text\n");
/// let error = MissingObjectsError::new(vec![(blob, ObjectKind::Blob)]);
/// assert_eq!(error.ids().collect::<Vec<_>>(), [blob]);
/// assert_eq!(
///     error.to_string(),
///     format!("blob {blob} is missing from this partial clone"),
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", describe_missing(.objects))]
#[non_exhaustive]
pub struct MissingObjectsError {
    /// Each missing object once, with the kind the operation expected, in the order it was needed.
    pub objects: Vec<(ObjectId, ObjectKind)>,
}

impl MissingObjectsError {
    /// Lists `objects` as missing.
    pub fn new(objects: Vec<(ObjectId, ObjectKind)>) -> Self {
        Self { objects }
    }

    /// The missing identities, in [`Self::objects`] order.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = ObjectId> + '_ {
        self.objects.iter().map(|&(id, _)| id)
    }
}

fn describe_missing(objects: &[(ObjectId, ObjectKind)]) -> String {
    let count = objects.len();
    let place = "this partial clone";
    match objects {
        [] => format!("no objects missing from {place}"),
        [(id, kind)] => format!("{} {id} is missing from {place}", kind.as_str()),
        [(id, kind), ..] => format!(
            "{count} objects missing from {place}, first {} {id}",
            kind.as_str()
        ),
    }
}

/// Stops collecting references at the first storage failure or unparseable payload.
enum Stop {
    Read(ObjectReadError),
    Malformed,
}

impl From<crate::edges::Error> for Stop {
    fn from(_: crate::edges::Error) -> Self {
        Self::Malformed
    }
}

impl Objects {
    /// Collects the identities that objects in promisor packs reference.
    ///
    /// Reads the commits, trees and tags of every pinned pack with a `.promisor` file, in this
    /// store and its alternates, and records the objects they name. Blob payloads are not read.
    ///
    /// As with Git, `.promisor` files only promise anything in a partial clone: this reader must
    /// come from a [`Repository`](crate::Repository) whose
    /// [`is_partial_clone`](crate::Repository::is_partial_clone) is true when it was opened.
    /// Otherwise, and for readers without promisor packs, the set is empty and nothing is read,
    /// so the cost falls only on partial clones. Each distinct referenced identity charges
    /// [`ListLimits::max_entries`]; each read uses `read`.
    ///
    /// Building the set is proportional to the promisor packs, so callers that only need it for
    /// missing objects should build it once, after the first missing object, and reuse it.
    ///
    /// # Errors
    ///
    /// Returns storage failures, [`ObjectReadError::Limit`] when the entry budget runs out,
    /// [`ObjectReadError::Corrupt`] when a promisor-pack commit, tree or tag does not parse, and
    /// [`ObjectReadError::Cancelled`].
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ListLimits, ObjectFormat, ReadLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let objects = repo.objects()?;
    /// let promised = objects.promised_objects(
    ///     ListLimits::default(),
    ///     ReadLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert!(promised.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn promised_objects(
        &self,
        limits: ListLimits,
        read: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<PromisedObjects, ObjectReadError> {
        /// Pack identities whose headers are located together, bounding the batch held at once.
        const BATCH: usize = 4096;
        let mut promised = PromisedObjects::default();
        if !self.partial_clone {
            return Ok(promised);
        }
        let mut remaining = limits.max_entries;
        let mut batch = Vec::with_capacity(BATCH);
        for pack in self.packs().filter(|pack| pack.is_promisor()) {
            let mut ids = pack.ids().peekable();
            while ids.peek().is_some() {
                batch.clear();
                batch.extend(ids.by_ref().take(BATCH));
                for (id, header) in self.headers_controlled(batch.drain(..), read, cancelled)? {
                    check_cancelled(cancelled)?;
                    if header.is_none_or(|header| header.kind() == ObjectKind::Blob) {
                        continue;
                    }
                    self.collect_references(id, read, cancelled, &mut promised, &mut remaining)?;
                }
            }
        }
        Ok(promised)
    }

    /// Adds the identities `id` references to `promised`, charging each new one to `remaining`.
    fn collect_references(
        &self,
        id: ObjectId,
        read: ReadLimits,
        cancelled: &AtomicBool,
        promised: &mut PromisedObjects,
        remaining: &mut usize,
    ) -> Result<(), ObjectReadError> {
        let object = self
            .read_controlled(id, read, cancelled)?
            .ok_or(ObjectReadError::Corrupt("promisor pack object disappeared"))?;
        let collected = crate::edges::visit(id, object.kind(), object.data(), |target, _| {
            if !promised.ids.contains(&target) {
                *remaining = remaining
                    .checked_sub(1)
                    .ok_or(Stop::Read(ObjectReadError::Limit("promised objects")))?;
                promised.ids.insert(target);
            }
            Ok::<_, Stop>(())
        });
        match collected {
            Ok(()) => Ok(()),
            Err(Stop::Read(error)) => Err(error),
            Err(Stop::Malformed) => Err(ObjectReadError::Corrupt("malformed promisor pack object")),
        }
    }
}

impl crate::Repository {
    /// Whether configuration marks this repository as a partial clone.
    ///
    /// True when `extensions.partialClone` is set, or some remote has `remote.<name>.promisor`
    /// true or any `remote.<name>.partialCloneFilter`, the settings `git clone --filter` and
    /// [`FetchRequest::filter`](crate::fetch::FetchRequest) write. As with Git, a filter makes
    /// its remote a promisor even when `promisor` is false. Without it, Git ignores `.promisor`
    /// pack files when classifying missing objects, and so do [`Objects::promised_objects`],
    /// [`Objects::walk_objects`] and fsck.
    ///
    /// This reads only the configuration already loaded with the repository. It never scans
    /// objects or packs, so retention and gc can call it on every run.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// assert!(!repo.is_partial_clone());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn is_partial_clone(&self) -> bool {
        let config = self.config();
        if config.value("extensions", None, "partialClone").is_some() {
            return true;
        }
        let remotes: HashSet<&[u8]> = config
            .entries()
            .iter()
            .filter(|entry| entry.section.eq_ignore_ascii_case(b"remote"))
            .filter_map(|entry| entry.subsection.as_deref())
            .collect();
        remotes.into_iter().any(|remote| {
            config
                .boolean("remote", Some(remote), "promisor")
                .is_ok_and(|promisor| promisor == Some(true))
                || config
                    .value("remote", Some(remote), "partialCloneFilter")
                    .is_some()
        })
    }
}
