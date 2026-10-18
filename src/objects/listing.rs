//! Enumeration of the stores, packs and object identities an [`Objects`] reader can see.
//!
//! Packed identities come from the reader's pinned indexes: listing them performs no I/O and
//! cannot fail. Loose identities come from scanning live fan-out directories, so that scan is
//! bounded by [`ListLimits`], cancellable and fallible.

use std::fs::{self, ReadDir};
use std::io;
use std::iter::FusedIterator;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::{Objects, Store};
use crate::pack::FilePack;
use crate::pack::reader::check_cancelled;
use crate::{ObjectFormat, ObjectId, ObjectReadError};

/// Marker files beside a pack, observed when the reader opened or refreshed.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PackMarks {
    pub(super) keep: bool,
    pub(super) promisor: bool,
}

/// Bounds for enumerating loose objects and collecting identities.
///
/// `max_entries` counts every directory entry a loose scan reads, including fan-out
/// directories and non-object files, and every identity [`Objects::object_ids`] collects. The
/// default suits repositories with tens of millions of objects; lower it for untrusted input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListLimits {
    /// Maximum directory entries read plus identities collected by one operation.
    pub max_entries: usize,
}

impl ListLimits {
    /// Sets [`Self::max_entries`].
    #[must_use]
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }
}

impl Default for ListLimits {
    fn default() -> Self {
        Self {
            max_entries: 1 << 27,
        }
    }
}

impl Objects {
    /// Returns this reader's object stores in search order: the primary store, then alternates.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, PackLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let stores: Vec<_> = objects.stores().collect();
    /// assert_eq!(stores.len(), 1);
    /// assert!(!stores[0].is_alternate());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn stores(&self) -> impl ExactSizeIterator<Item = ObjectStore<'_>> {
        let format = self.format;
        self.stores
            .iter()
            .enumerate()
            .map(move |(position, store)| ObjectStore {
                store,
                format,
                alternate: position != 0,
            })
    }

    /// Returns every pinned pack in search order, across the primary store and alternates.
    ///
    /// Equivalent to flattening [`ObjectStore::packs`] over [`Self::stores`].
    pub fn packs(&self) -> impl Iterator<Item = IndexedPack<'_>> {
        self.stores().flat_map(|store| store.packs())
    }

    /// Collects every object identity visible to this reader, sorted and without duplicates.
    ///
    /// Includes all pinned packs and the current loose objects of every store, alternates
    /// included, as [`git cat-file --batch-all-objects`] lists them. The implicit empty tree is
    /// not included unless it is stored. See [`Self::object_ids_controlled`] for limits.
    ///
    /// [`git cat-file --batch-all-objects`]: https://git-scm.com/docs/git-cat-file
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::object_ids_controlled`].
    ///
    /// ```
    /// use girt::{InitKind, ListLimits, ObjectFormat, PackLimits, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repo.loose_objects().write_blob(b"listed\n")?;
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// assert_eq!(objects.object_ids(ListLimits::default())?, [id]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn object_ids(&self, limits: ListLimits) -> Result<Vec<ObjectId>, ObjectReadError> {
        self.object_ids_controlled(limits, &AtomicBool::new(false))
    }

    /// Collects every visible identity as [`Self::object_ids`] does, checking `cancelled` between
    /// directory entries.
    ///
    /// Packed identities and loose directory entries both charge [`ListLimits::max_entries`].
    /// The loose scan has the live semantics of [`ObjectStore::loose_objects`].
    ///
    /// # Errors
    ///
    /// Returns [`ObjectReadError::Limit`] when the entry budget runs out,
    /// [`ObjectReadError::Cancelled`], or a loose directory I/O error with its path.
    pub fn object_ids_controlled(
        &self,
        limits: ListLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<ObjectId>, ObjectReadError> {
        let mut remaining = limits.max_entries;
        let mut ids = Vec::new();
        for store in self.stores() {
            for pack in store.packs() {
                check_cancelled(cancelled)?;
                remaining = remaining
                    .checked_sub(pack.object_count())
                    .ok_or(ObjectReadError::Limit("listed object entries"))?;
                ids.extend(pack.ids());
            }
            let mut loose = store.loose_objects_controlled(
                ListLimits::default().with_max_entries(remaining),
                cancelled,
            );
            for object in &mut loose {
                ids.push(object?.id());
            }
            remaining = loose.remaining;
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }
}

/// One object directory of an [`Objects`] reader: the primary store or an alternate.
///
/// Alternates are borrowed storage; their contents are never pruning candidates for this
/// repository.
#[derive(Debug, Clone, Copy)]
pub struct ObjectStore<'a> {
    store: &'a Store,
    format: ObjectFormat,
    alternate: bool,
}

impl<'a> ObjectStore<'a> {
    /// Canonical path of this `objects` directory.
    pub fn directory(&self) -> &'a Path {
        &self.store.directory
    }

    /// Whether this store is an alternate rather than the repository's own primary store.
    pub fn is_alternate(&self) -> bool {
        self.alternate
    }

    /// Returns this store's pinned packs in search order (sorted by index filename).
    pub fn packs(&self) -> impl ExactSizeIterator<Item = IndexedPack<'a>> + use<'a> {
        let store = self.store;
        let format = self.format;
        store
            .packs
            .iter()
            .zip(&store.pack_paths)
            .zip(&store.pack_marks)
            .map(move |((pack, path), marks)| IndexedPack {
                pack,
                path,
                marks: *marks,
                format,
            })
    }

    /// Scans this store's loose objects with [`ListLimits::default`] and no cancellation.
    ///
    /// See [`Self::loose_objects_controlled`].
    pub fn loose_objects(&self) -> LooseIds<'a> {
        self.loose_objects_controlled(ListLimits::default(), &NEVER_CANCELLED)
    }

    /// Scans this store's loose objects lazily, one directory entry at a time.
    ///
    /// Yields each regular file under a two-hex-digit fan-out directory whose name completes a
    /// full identity in this store's format. Other names, such as temporary files, and
    /// directories or symlinks with object names are skipped without error, as Git skips them.
    /// Order follows the file system and is unspecified; sort if order matters. No file is
    /// opened, so a yielded entry proves only that its name exists, not that its contents are
    /// valid.
    ///
    /// The scan is live, unlike the reader's pinned packs: files written or removed during the
    /// scan may or may not appear, and a fan-out directory removed meanwhile is skipped. A
    /// missing `objects` directory yields nothing. Every directory entry read charges
    /// [`ListLimits::max_entries`], and `cancelled` is checked before each one.
    ///
    /// The iterator yields an error at most once, then ends: [`ObjectReadError::Limit`],
    /// [`ObjectReadError::Cancelled`], or [`ObjectReadError::Path`] for directory I/O.
    pub fn loose_objects_controlled(
        &self,
        limits: ListLimits,
        cancelled: &'a AtomicBool,
    ) -> LooseIds<'a> {
        LooseIds {
            directory: &self.store.directory,
            format: self.format,
            cancelled,
            remaining: limits.max_entries,
            state: State::Start,
        }
    }
}

static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// A pack/index pair pinned by an [`Objects`] reader.
///
/// Every value describes the snapshot taken when the reader opened or last refreshed, not the
/// current file system.
#[derive(Debug, Clone, Copy)]
pub struct IndexedPack<'a> {
    pack: &'a FilePack,
    path: &'a PathBuf,
    marks: PackMarks,
    format: ObjectFormat,
}

impl<'a> IndexedPack<'a> {
    /// Path of the `.pack` file.
    pub fn path(&self) -> &'a Path {
        self.path
    }

    /// Path of the `.idx` file paired with [`Self::path`].
    pub fn index_path(&self) -> PathBuf {
        self.path.with_extension("idx")
    }

    /// Number of objects the index lists.
    pub fn object_count(&self) -> usize {
        self.pack.object_count()
    }

    /// Size of the `.pack` file in bytes.
    pub fn pack_bytes(&self) -> u64 {
        self.pack.artifact_bytes().0
    }

    /// Size of the `.idx` file in bytes.
    pub fn index_bytes(&self) -> u64 {
        self.pack.artifact_bytes().1
    }

    /// Pack checksum from the trailer, which Git also uses to name the pack.
    ///
    /// Opening checks that the index and pack trailers agree; it does not hash the pack. Use
    /// [`Objects::verify_packs`] for that proof.
    pub fn checksum(&self) -> ObjectId {
        self.pack.checksum()
    }

    /// Whether a `.keep` file protected this pack from repacking.
    pub fn is_kept(&self) -> bool {
        self.marks.keep
    }

    /// Whether a `.promisor` file marked this pack as fetched from a promisor remote.
    pub fn is_promisor(&self) -> bool {
        self.marks.promisor
    }

    /// Object format of the identities in this pack.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

    /// Whether this pack's index lists `id`, found by binary search of the pinned index.
    ///
    /// Reads no pack file and does not check that the packed entry is readable.
    ///
    /// # Errors
    ///
    /// Index structure is checked lazily, so a search that reaches a corrupt or truncated part of
    /// the index returns an [`ObjectReadError`] rather than reporting the object absent.
    pub fn contains(&self, id: ObjectId) -> Result<bool, ObjectReadError> {
        Ok(self.pack.find(id)?.is_some())
    }

    /// Iterates this pack's identities in index order, which is ascending identity order.
    ///
    /// Reads only the pinned in-memory index: no I/O, no allocation per identity.
    pub fn ids(&self) -> PackIds<'a> {
        PackIds {
            pack: self.pack,
            positions: 0..self.pack.object_count(),
        }
    }
}

/// Identities of one [`IndexedPack`] in ascending order; see [`IndexedPack::ids`].
#[derive(Debug, Clone)]
pub struct PackIds<'a> {
    pack: &'a FilePack,
    positions: Range<usize>,
}

impl Iterator for PackIds<'_> {
    type Item = ObjectId;

    fn next(&mut self) -> Option<ObjectId> {
        self.positions
            .next()
            .map(|position| self.pack.identity_at(position))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.positions.size_hint()
    }

    fn nth(&mut self, n: usize) -> Option<ObjectId> {
        self.positions
            .nth(n)
            .map(|position| self.pack.identity_at(position))
    }
}

impl DoubleEndedIterator for PackIds<'_> {
    fn next_back(&mut self) -> Option<ObjectId> {
        self.positions
            .next_back()
            .map(|position| self.pack.identity_at(position))
    }
}

impl ExactSizeIterator for PackIds<'_> {}
impl FusedIterator for PackIds<'_> {}

/// A loose object file found by [`ObjectStore::loose_objects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LooseObject {
    id: ObjectId,
    path: PathBuf,
}

impl LooseObject {
    /// Identity named by the file's path.
    pub fn id(&self) -> ObjectId {
        self.id
    }

    /// Path of the loose object file; it may have been removed since the scan read it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Lazy, bounded scan of one store's loose objects; see [`ObjectStore::loose_objects_controlled`].
#[derive(Debug)]
pub struct LooseIds<'a> {
    directory: &'a Path,
    format: ObjectFormat,
    cancelled: &'a AtomicBool,
    remaining: usize,
    state: State,
}

#[derive(Debug)]
enum State {
    Start,
    Fanouts(ReadDir),
    Objects {
        fanouts: ReadDir,
        prefix: String,
        directory: PathBuf,
        // Boxed: a Windows `ReadDir` holds its find data inline, so two would dwarf the others.
        entries: Box<ReadDir>,
    },
    Done,
}

impl LooseIds<'_> {
    fn charge(&mut self) -> Result<(), ObjectReadError> {
        check_cancelled(self.cancelled)?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(ObjectReadError::Limit("loose directory entries"))?;
        Ok(())
    }

    fn advance(&mut self) -> Result<Option<LooseObject>, ObjectReadError> {
        loop {
            match std::mem::replace(&mut self.state, State::Done) {
                State::Done => return Ok(None),
                State::Start => match fs::read_dir(self.directory) {
                    Ok(fanouts) => self.state = State::Fanouts(fanouts),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(source) => return Err(at(self.directory, source)),
                },
                State::Fanouts(mut fanouts) => {
                    let Some(entry) = fanouts.next() else {
                        return Ok(None);
                    };
                    self.charge()?;
                    let entry = entry.map_err(|source| at(self.directory, source))?;
                    let name = entry.file_name();
                    let prefix = name
                        .to_str()
                        .filter(|name| name.len() == 2 && name.bytes().all(is_lower_hex));
                    let directory = entry.path();
                    let entries = match prefix.map(|_| fs::read_dir(&directory)) {
                        Some(Ok(entries)) => Some(entries),
                        // Not a directory, or removed since listing: neither holds objects.
                        Some(Err(error))
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                            ) =>
                        {
                            None
                        }
                        Some(Err(source)) => return Err(at(&directory, source)),
                        None => None,
                    };
                    self.state = match (prefix, entries) {
                        (Some(prefix), Some(entries)) => State::Objects {
                            fanouts,
                            prefix: prefix.to_owned(),
                            directory,
                            entries: Box::new(entries),
                        },
                        _ => State::Fanouts(fanouts),
                    };
                }
                State::Objects {
                    fanouts,
                    mut prefix,
                    directory,
                    mut entries,
                } => {
                    let Some(entry) = entries.next() else {
                        self.state = State::Fanouts(fanouts);
                        continue;
                    };
                    self.charge()?;
                    let entry = entry.map_err(|source| at(&directory, source))?;
                    let name = entry.file_name();
                    let found = name
                        .to_str()
                        .filter(|suffix| {
                            suffix.len() == self.format.digest_len() * 2 - 2
                                && suffix.bytes().all(is_lower_hex)
                        })
                        .and_then(|suffix| {
                            prefix.push_str(suffix);
                            let id = ObjectId::from_hex(self.format, &prefix).ok();
                            prefix.truncate(2);
                            id
                        });
                    // Git counts directories and symlinks with object names as garbage.
                    let found = match found {
                        Some(id) => entry
                            .file_type()
                            .map_err(|source| at(&directory, source))?
                            .is_file()
                            .then_some(id),
                        None => None,
                    };
                    let path = found.is_some().then(|| entry.path());
                    self.state = State::Objects {
                        fanouts,
                        prefix,
                        directory,
                        entries,
                    };
                    if let (Some(id), Some(path)) = (found, path) {
                        return Ok(Some(LooseObject { id, path }));
                    }
                }
            }
        }
    }
}

impl Iterator for LooseIds<'_> {
    type Item = Result<LooseObject, ObjectReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        // `advance` leaves `Done` behind on errors, so an error ends the scan.
        self.advance().transpose()
    }
}

impl FusedIterator for LooseIds<'_> {}

fn is_lower_hex(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn at(path: &Path, source: io::Error) -> ObjectReadError {
    ObjectReadError::Path {
        path: path.to_owned(),
        source,
    }
}
