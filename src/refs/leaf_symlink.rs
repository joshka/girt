//! Explicit filesystem leaf-link reads, separate from strict reference publication.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use super::store::{check_path, io_error, parse_loose};
use super::{Backend, RefName, ReferenceError, ReferenceObservation, References, Target, packed};

impl References<'_> {
    /// Reads one reference while explicitly permitting final-file filesystem symlinks.
    ///
    /// Relative link destinations are relative to their containing directory. Link text such as
    /// `refs/heads/main` is a filesystem path, not Git's legacy Git-directory-relative symbolic
    /// reference syntax. Only `ref: ...` file contents produce a symbolic [`Target`]. Symlink
    /// ancestors of the original name or any followed destination remain unsupported; destinations
    /// outside the repository are otherwise permitted. This method never changes write/CAS policy.
    ///
    /// A successful loose read retains the requested name and clears the packed peeled hint.
    /// Missing paths and followed links ending at a directory permit normal packed fallback.
    /// Malformed contents and other I/O errors do not. An original directory remains a conflict.
    /// Per-worktree names never fall back to packed storage. Observations are owned but live reads
    /// are not atomic across link hops; this is not a sandbox against adversarial path replacement.
    ///
    /// `max_symlink_hops` bounds followed links, including cycles. `max_bytes` bounds aggregate
    /// link destination bytes and file bytes, including packed fallback. Files are read with at
    /// most one excess byte to detect exhaustion. Reftable uses [`Self::read_observation`] and
    /// its configured stack limits instead; these filesystem limits do not apply there.
    ///
    /// # Errors
    ///
    /// Reports unsupported ancestors/nonregular files, I/O and malformed data, namespace conflicts,
    /// and [`ReferenceError::Limit`] on exhausted byte or link budgets. No storage is modified.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    /// let observation = repo
    ///     .references()?
    ///     .read_observation_following_leaf_symlinks(&RefName::new("HEAD")?, 32, 64 * 1024)?;
    /// assert!(observation.is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_observation_following_leaf_symlinks(
        &self,
        name: &RefName,
        max_symlink_hops: usize,
        max_bytes: usize,
    ) -> Result<Option<ReferenceObservation>, ReferenceError> {
        if self.reference_backend == Backend::Reftable {
            return self.read_observation(name);
        }
        let mut path = self.path(name)?;
        let mut remaining = max_bytes;
        let mut hops = 0;
        loop {
            check_ancestors(&path)?;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(source) => return Err(io_error(&path, source)),
            };
            if metadata.file_type().is_symlink() {
                if hops == max_symlink_hops {
                    return Err(ReferenceError::Limit("reference filesystem symlink hops"));
                }
                let destination = fs::read_link(&path).map_err(|source| io_error(&path, source))?;
                remaining = remaining
                    .checked_sub(destination.as_os_str().as_encoded_bytes().len())
                    .ok_or(ReferenceError::Limit("reference bytes"))?;
                path = path
                    .parent()
                    .expect("reference path has a parent")
                    .join(destination);
                hops += 1;
                continue;
            }
            if metadata.is_dir() {
                if hops != 0 {
                    break;
                }
                return Err(ReferenceError::Conflict(path));
            }
            if !metadata.is_file() {
                return Err(ReferenceError::Unsupported("non-regular reference file"));
            }
            let Some(bytes) = read_bounded(&path, &mut remaining)? else {
                break;
            };
            return Ok(Some(ReferenceObservation {
                name: name.clone(),
                target: parse_loose(self.object_format, &bytes, &path)?,
                peeled_hint: None,
            }));
        }
        if name.per_worktree() {
            return Ok(None);
        }
        let packed_path = self.common_dir.join("packed-refs");
        check_path(&packed_path)?;
        let bytes = read_bounded(&packed_path, &mut remaining)?.unwrap_or_default();
        let records = packed::parse(self.object_format, &bytes, &packed_path)?;
        Ok(records.get(name).map(|record| ReferenceObservation {
            name: name.clone(),
            target: Target::Direct(record.target),
            peeled_hint: record.peeled_hint,
        }))
    }
}

fn check_ancestors(path: &Path) -> Result<(), ReferenceError> {
    for ancestor in path.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ReferenceError::Unsupported(
                    "filesystem symlink in reference ancestor",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ReferenceError::Conflict(ancestor.into()));
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(source) => return Err(io_error(ancestor, source)),
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, remaining: &mut usize) -> Result<Option<Vec<u8>>, ReferenceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let mut bytes = Vec::new();
    file.take(remaining.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    *remaining = remaining
        .checked_sub(bytes.len())
        .ok_or(ReferenceError::Limit("reference bytes"))?;
    Ok(Some(bytes))
}

#[cfg(all(test, unix))]
mod tests;
