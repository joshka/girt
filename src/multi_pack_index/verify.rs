//! Explicit verification reports auxiliary-index failures instead of treating them as absent hints.
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Limits, MultiPackIndex};
use crate::{ObjectId, Repository};

impl Repository {
    /// Verifies the repository's standalone MIDX or incremental chain against its named pack
    /// indexes.
    ///
    /// Explicit verification ignores `core.multiPackIndex`: disabling acceleration does not disable
    /// diagnostics. A standalone file takes precedence over a chain. Whole-pack payload
    /// verification remains separate through [`crate::Objects::verify_packs`]. No files are
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MissingIndex`] when no index exists, or structured parsing, pack and I/O
    /// errors. Uses default parser and object-store bounds; see
    /// [`Self::verify_multi_pack_index_with`].
    pub fn verify_multi_pack_index(&self) -> Result<(), Error> {
        self.verify_multi_pack_index_with(Limits::default(), &AtomicBool::new(false))
    }

    /// Verifies with aggregate MIDX byte/object bounds and cancellation between layers.
    ///
    /// At most 256 chain layers are read. Snapshot opening uses default pack/alternate bounds;
    /// callers with larger snapshots can parse layers and call [`MultiPackIndex::verify`] directly.
    /// Hashing and one bounded parser invocation cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::verify_multi_pack_index`], including [`Error::Cancelled`]
    /// before I/O, between layers and after validation.
    pub fn verify_multi_pack_index_with(
        &self,
        mut limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        check(cancel)?;
        let directory = self.object_dir().join("pack");
        if let Some(bytes) = read(&directory.join("multi-pack-index"), limits.max_bytes)? {
            let index = MultiPackIndex::parse_with(&bytes, self.object_format(), limits)?;
            let objects =
                self.objects_controlled(Default::default(), Default::default(), cancel)?;
            index.verify(&objects)?;
            return check(cancel);
        }
        let directory = directory.join("multi-pack-index.d");
        let chain = read(
            &directory.join("multi-pack-index-chain"),
            256 * (self.object_format().digest_len() * 2 + 1),
        )?
        .ok_or(Error::MissingIndex)?;
        let chain = std::str::from_utf8(&chain).map_err(|_| Error::Malformed("chain encoding"))?;
        let objects = self.objects_controlled(Default::default(), Default::default(), cancel)?;
        let mut seen = std::collections::BTreeSet::new();
        for name in chain.lines() {
            check(cancel)?;
            if seen.len() >= 256 {
                return Err(Error::Limit("chain layers"));
            }
            let hash = ObjectId::from_hex(self.object_format(), name)
                .map_err(|_| Error::Malformed("chain identity"))?;
            if !seen.insert(hash) {
                return Err(Error::Malformed("duplicate chain layer"));
            }
            let path = directory.join(format!("multi-pack-index-{hash}.midx"));
            let bytes = read(&path, limits.max_bytes)?.ok_or(Error::MissingIndex)?;
            let layer = MultiPackIndex::parse_with(&bytes, self.object_format(), limits)?;
            let end = bytes.len() - self.object_format().digest_len();
            if bytes[end..] != *hash.as_bytes() {
                return Err(Error::Malformed("chain filename checksum"));
            }
            layer.verify(&objects)?;
            limits.max_bytes -= bytes.len();
            limits.max_objects -= layer.len();
            check(cancel)?;
        }
        if seen.is_empty() {
            return Err(Error::Malformed("empty chain"));
        }
        Ok(())
    }
}

fn read(path: &Path, max_bytes: usize) -> Result<Option<Vec<u8>>, Error> {
    let at = |source| Error::Path {
        path: path.to_owned(),
        source,
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(at(error)),
    };
    if file.metadata().map_err(at)?.len() > max_bytes as u64 {
        return Err(Error::Limit("bytes"));
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(at)?;
    if bytes.len() > max_bytes {
        return Err(Error::Limit("bytes"));
    }
    Ok(Some(bytes))
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(())
}
