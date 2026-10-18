//! Metadata attached to objects without changing their identities.
//!
//! A notes ref points to an ordinary commit whose tree maps annotated object IDs to note blobs.
//! [`Notes`] reads those mappings, including nested hexadecimal fanout directories, and composes
//! edits in memory. [`Notes::prepare`] builds a new notes commit; [`Prepared::store`] installs its
//! objects but never updates a reference. Publish the resulting commit through a conditional
//! reference transaction, checking the tip originally read. Notes can annotate any object kind.
//!
//! ```
//! use girt::notes::{Notes, Operation};
//! use girt::{ObjectFormat, ObjectId, Signature};
//! let format = ObjectFormat::Sha1;
//! let object = ObjectId::for_blob(format, b"reviewed content");
//! let mut notes = Notes::new(format);
//! notes.add(object, b"Reviewed-by: A. Writer\n")?;
//! assert_eq!(
//!     notes.get(object),
//!     Some(b"Reviewed-by: A. Writer\n".as_slice())
//! );
//! let person = Signature {
//!     name: b"A".to_vec(),
//!     email: b"a@b".to_vec(),
//!     seconds: 1,
//!     offset_minutes: 0,
//! };
//! let prepared = notes.prepare(None, person.clone(), person, Operation::Add)?;
//! assert!(prepared.commit().parents().is_empty());
//! # Ok::<(), girt::notes::Error>(())
//! ```
//!
//! Note bytes are literal: unlike `git notes -m`, these operations do not strip comments or
//! whitespace, nor add a final newline. Appending inserts one blank-line separator. Output trees
//! are flat; imported fanout or uppercase paths cannot be prepared for writing, preserving layout.
//! Non-note entries and duplicate mappings are explicitly refused on import, so edits cannot
//! silently discard them. Notes merging and automatic rewrite propagation are not performed.

mod configuration;
use std::collections::BTreeMap;

pub use configuration::Selection;

use crate::{
    Commit, CommitFields, EntryMode, ObjectFormat, ObjectId, Objects, Signature, Tree, TreeEntry,
};

/// An in-memory collection of literal note bytes in one object format.
#[derive(Clone, Debug)]
pub struct Notes {
    format: ObjectFormat,
    notes: BTreeMap<ObjectId, Vec<u8>>,
    preserve_layout: bool,
}

/// A failed notes operation; references are never changed by this module.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The object ID belongs to a different hash format.
    #[error("notes object format mismatch")]
    Format,
    /// A note already exists at the destination.
    #[error("note already exists for {0}")]
    Exists(ObjectId),
    /// No note exists at the source.
    #[error("no note exists for {0}")]
    Missing(ObjectId),
    /// A tree cannot be represented as an unambiguous notes collection.
    #[error("unsupported notes tree: {0}")]
    Layout(&'static str),
    /// The configured primary reference is invalid or outside the notes namespace.
    #[error("invalid notes reference {name:?}")]
    Reference {
        /// Configured reference bytes.
        name: Vec<u8>,
        /// Syntax failure, absent when the namespace is unsupported.
        #[source]
        source: Option<crate::refs::InvalidRefName>,
    },
    /// The collection exceeded its import bounds.
    #[error("notes import limit exceeded")]
    Limit,
    /// An underlying object operation failed.
    #[error("notes object operation: {0}")]
    Object(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The operation recorded in a Git-compatible notes commit message.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Operation {
    /// Add or replace a note.
    Add,
    /// Append content to a note.
    Append,
    /// Copy a note to another object.
    Copy,
    /// Remove a note.
    Remove,
}
impl Operation {
    fn message(self) -> &'static [u8] {
        match self {
            Self::Add => b"Notes added by 'git notes add'\n",
            Self::Append => b"Notes added by 'git notes append'\n",
            Self::Copy => b"Notes added by 'git notes copy'\n",
            Self::Remove => b"Notes removed by 'git notes remove'\n",
        }
    }
}

impl Notes {
    /// Creates an empty notes collection without accessing storage.
    pub fn new(format: ObjectFormat) -> Self {
        Self {
            format,
            notes: BTreeMap::new(),
            preserve_layout: false,
        }
    }

    /// Reads a notes commit, including any depth of hexadecimal fanout.
    ///
    /// Import admits at most 100,000 tree entries and 64 MiB of note bytes. Each object also uses
    /// the default object read bounds. References are caller-selected and are not read here.
    ///
    /// # Errors
    ///
    /// Reports missing/corrupt objects, exhausted bounds, non-note entries, duplicate mappings,
    /// and entries with non-blob leaf modes. Nothing is written.
    pub fn read(objects: &Objects, commit: ObjectId) -> Result<Self, Error> {
        let commit_value = objects.read_commit(commit).map_err(object_error)?;
        let mut result = Self::new(commit.format());
        let mut pending = vec![(commit_value.tree(), Vec::new())];
        let mut entries = 0usize;
        let mut bytes = 0usize;
        let width = commit.to_string().len();
        while let Some((id, prefix)) = pending.pop() {
            let tree = objects.read_tree(id).map_err(object_error)?;
            tree.validate().map_err(object_error)?;
            for entry in tree.entries() {
                entries += 1;
                if entries > 100_000 {
                    return Err(Error::Limit);
                }
                let mut hex = prefix.clone();
                hex.extend_from_slice(&entry.name);
                if hex.len() > width || !hex.iter().all(u8::is_ascii_hexdigit) {
                    return Err(Error::Layout("non-note entry"));
                }
                if hex.iter().any(u8::is_ascii_uppercase) {
                    result.preserve_layout = true;
                }
                if entry.mode == EntryMode::Tree && hex.len() < width {
                    result.preserve_layout = true;
                    pending.push((entry.id, hex));
                } else if entry.mode == EntryMode::Blob && hex.len() == width {
                    let text =
                        std::str::from_utf8(&hex).map_err(|_| Error::Layout("non-hex name"))?;
                    let object = ObjectId::from_hex(commit.format(), text).map_err(object_error)?;
                    let note = objects.read_blob(entry.id).map_err(object_error)?;
                    bytes = bytes.checked_add(note.len()).ok_or(Error::Limit)?;
                    if bytes > 64 * 1024 * 1024 {
                        return Err(Error::Limit);
                    }
                    if result.notes.insert(object, note).is_some() {
                        return Err(Error::Layout("duplicate note mapping"));
                    }
                } else {
                    return Err(Error::Layout("invalid note mode or fanout width"));
                }
            }
        }
        Ok(result)
    }

    /// Returns literal note bytes, or `None` when no note is attached.
    pub fn get(&self, object: ObjectId) -> Option<&[u8]> {
        self.notes.get(&object).map(Vec::as_slice)
    }

    /// Lists annotated object IDs and note bytes in ascending object-ID order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (ObjectId, &[u8])> {
        self.notes.iter().map(|(&id, note)| (id, note.as_slice()))
    }

    /// Adds a note, refusing to overwrite an existing mapping. Empty bytes are a valid note.
    ///
    /// # Errors
    ///
    /// Reports a format mismatch or an existing destination; the collection remains unchanged.
    pub fn add(&mut self, object: ObjectId, note: impl AsRef<[u8]>) -> Result<(), Error> {
        self.check_format(object)?;
        if self.notes.contains_key(&object) {
            return Err(Error::Exists(object));
        }
        self.notes.insert(object, note.as_ref().to_vec());
        Ok(())
    }

    /// Sets literal note bytes, returning the previous note when present.
    ///
    /// # Errors
    ///
    /// Reports a format mismatch without changing the collection.
    pub fn set(
        &mut self,
        object: ObjectId,
        note: impl AsRef<[u8]>,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.check_format(object)?;
        Ok(self.notes.insert(object, note.as_ref().to_vec()))
    }

    /// Appends bytes, separating nonempty old and new content with a blank line.
    ///
    /// An existing final LF is retained; otherwise one is inserted before the separator LF.
    /// An absent note is created. Empty new content leaves an existing note unchanged.
    ///
    /// # Errors
    ///
    /// Reports a format mismatch without changing the collection.
    pub fn append(&mut self, object: ObjectId, note: impl AsRef<[u8]>) -> Result<(), Error> {
        self.check_format(object)?;
        let previous = self.notes.entry(object).or_default();
        let note = note.as_ref();
        if !previous.is_empty() && !note.is_empty() {
            if !previous.ends_with(b"\n") {
                previous.push(b'\n');
            }
            previous.push(b'\n');
        }
        previous.extend_from_slice(note);
        Ok(())
    }

    /// Copies a source note to an unoccupied destination.
    ///
    /// # Errors
    ///
    /// Reports a missing source, occupied destination or format mismatch; nothing changes.
    pub fn copy(&mut self, source: ObjectId, destination: ObjectId) -> Result<(), Error> {
        self.check_format(source)?;
        self.check_format(destination)?;
        let note = self.get(source).ok_or(Error::Missing(source))?.to_vec();
        self.add(destination, note)
    }

    /// Removes a note and returns its bytes, or `None` if absent.
    pub fn remove(&mut self, object: ObjectId) -> Option<Vec<u8>> {
        self.notes.remove(&object)
    }

    /// Computes a flat notes tree and commit with an explicit previous tip and identities.
    ///
    /// This always computes a commit, including for an unchanged collection. Callers choose
    /// whether to store/publish it. No objects or references are written. The parent is not read.
    ///
    /// # Errors
    ///
    /// Reports imported fanout/uppercase paths, parent format mismatch or invalid signatures.
    pub fn prepare(
        &self,
        parent: Option<ObjectId>,
        author: Signature,
        committer: Signature,
        operation: Operation,
    ) -> Result<Prepared, Error> {
        if self.preserve_layout {
            return Err(Error::Layout(
                "writing imported fanout or uppercase paths is not supported",
            ));
        }
        if let Some(parent) = parent {
            self.check_format(parent)?;
        }
        let blobs: Vec<_> = self
            .notes
            .values()
            .map(|bytes| (ObjectId::for_blob(self.format, bytes), bytes.clone()))
            .collect();
        let entries = self
            .notes
            .keys()
            .zip(&blobs)
            .map(|(object, (id, _))| TreeEntry {
                mode: EntryMode::Blob,
                name: object.to_string().into_bytes(),
                id: *id,
            })
            .collect();
        let tree = Tree::new(self.format, entries).map_err(object_error)?;
        let commit = Commit::new(CommitFields {
            tree: tree.id(),
            parents: parent.into_iter().collect(),
            author,
            committer,
            extra_headers: vec![],
            message: operation.message().to_vec(),
        })
        .map_err(object_error)?;
        Ok(Prepared {
            commit,
            tree,
            blobs,
        })
    }

    fn check_format(&self, id: ObjectId) -> Result<(), Error> {
        if id.format() != self.format {
            return Err(Error::Format);
        }
        Ok(())
    }
}

/// Computed notes objects awaiting explicit storage and reference publication.
///
/// Owns a copy of all note bytes in addition to the source collection. Identical note contents may
/// appear more than once; storing them repeatedly is harmless under content-addressed storage.
#[derive(Clone, Debug)]
pub struct Prepared {
    commit: Commit,
    tree: Tree,
    blobs: Vec<(ObjectId, Vec<u8>)>,
}
impl Prepared {
    /// Returns the computed notes commit, whose ID is the proposed reference target.
    pub fn commit(&self) -> &Commit {
        &self.commit
    }
    /// Returns the computed flat notes tree.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }
    /// Stores blobs, then tree, then commit. Never updates a ref or removes old objects.
    ///
    /// # Errors
    ///
    /// Reports a storage format mismatch or write failure. Earlier successfully written objects
    /// may remain unreachable; retrying the same artifact is safe under the object-store contract.
    pub fn store(&self, destination: &crate::LooseObjects) -> Result<ObjectId, Error> {
        if destination.object_format() != self.commit.id().format() {
            return Err(Error::Format);
        }
        for (_, bytes) in &self.blobs {
            destination.write_blob(bytes).map_err(object_error)?;
        }
        destination.write_tree(&self.tree).map_err(object_error)?;
        destination.write_commit(&self.commit).map_err(object_error)
    }
}

fn object_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Object(Box::new(error))
}
