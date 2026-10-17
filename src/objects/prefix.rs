//! Prefix discovery without decoding object payloads.
use std::{fs, io};

use super::{ObjectReadError, Objects};
use crate::{ObjectFormatError, ObjectId, ObjectIdPrefix};

/// Failure to select one stored identity from a prefix.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolvePrefixError {
    /// No stored identity matches.
    #[error("no object matches {0}")]
    Missing(ObjectIdPrefix),
    /// More than one distinct identity matches, in ascending order.
    #[error("ambiguous object prefix {prefix}: {candidates:?}")]
    Ambiguous {
        /// The requested prefix.
        prefix: ObjectIdPrefix,
        /// All distinct matching identities, sorted by ID.
        candidates: Vec<ObjectId>,
    },
    /// Storage could not be inspected.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

impl Objects {
    /// Finds all distinct stored identities matching a prefix, in ascending order.
    ///
    /// Searches loose filenames and pinned pack indexes in the primary store and alternates.
    /// New loose files are visible; new packs require [`Self::refresh`]. Payloads are not decoded
    /// or hash-verified, and unindexed packs are ignored, just as for ordinary object lookup.
    ///
    /// # Errors
    ///
    /// Reports format mismatch, directory I/O, or a changed/truncated pinned index.
    pub fn prefix_matches(&self, prefix: ObjectIdPrefix) -> Result<Vec<ObjectId>, ObjectReadError> {
        self.require_prefix_format(prefix)?;
        let mut ids = Vec::new();
        let text = prefix.to_string();
        let directory_name = &text[..2];
        for store in &self.stores {
            let directory = store.directory.join(directory_name);
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(ObjectReadError::Path {
                        path: directory,
                        source,
                    });
                }
            };
            if let Some(entries) = entries {
                for entry in entries {
                    let entry = entry.map_err(|source| ObjectReadError::Path {
                        path: directory.clone(),
                        source,
                    })?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if name.len() != self.format.digest_len() * 2 - 2 || !name.starts_with(&text[2..]) {
                        continue;
                    }
                    let Ok(id) =
                        ObjectId::from_hex(self.format, &format!("{directory_name}{name}"))
                    else {
                        continue;
                    };
                    if prefix.matches(id) && entry.path().is_file() {
                        ids.push(id);
                    }
                }
            }
            for pack in &store.packs {
                ids.extend(pack.prefix_matches(prefix)?);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Resolves a prefix to exactly one stored identity, independently of revision syntax.
    ///
    /// # Errors
    ///
    /// Returns [`ResolvePrefixError::Missing`] for no match and
    /// [`ResolvePrefixError::Ambiguous`] with every candidate for multiple matches.
    /// Storage and format errors are preserved as [`ResolvePrefixError::Read`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, ObjectIdPrefix, PackLimits, Repository};
    /// let dir = tempfile::tempdir()?;
    /// let repo = Repository::init(ObjectFormat::Sha1, dir.path().join("repo"), InitKind::Bare)?;
    /// let id = repo.loose_objects().write_blob(b"hello")?;
    /// let objects = repo.objects(PackLimits::default())?;
    /// let prefix = ObjectIdPrefix::from_hex(repo.object_format(), "b6fc4")?;
    /// assert_eq!(objects.resolve_prefix(prefix)?, id);
    /// assert_eq!(objects.abbreviate(id, 4)?.to_string(), "b6fc");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve_prefix(&self, prefix: ObjectIdPrefix) -> Result<ObjectId, ResolvePrefixError> {
        let candidates = self.prefix_matches(prefix)?;
        match candidates.as_slice() {
            [] => Err(ResolvePrefixError::Missing(prefix)),
            [id] => Ok(*id),
            _ => Err(ResolvePrefixError::Ambiguous { prefix, candidates }),
        }
    }

    /// Finds the shortest prefix distinguishing `id` from all other stored identities.
    ///
    /// Clamps `min_len` to four through the full format width. The ID need not exist (for example,
    /// a diff's null sentinel); uniqueness is relative to the current storage view. Retain this
    /// reader across calls to reuse its pack snapshot. Concurrent writes can invalidate uniqueness.
    ///
    /// # Errors
    ///
    /// Reports format mismatch or storage inspection failure, as [`Self::prefix_matches`] does.
    pub fn abbreviate(
        &self,
        id: ObjectId,
        min_len: usize,
    ) -> Result<ObjectIdPrefix, ObjectReadError> {
        id.require_format(self.format).map_err(crate::Error::from)?;
        let hex = id.to_string();
        let mut length = min_len.clamp(4, hex.len());
        let prefix = ObjectIdPrefix::from_hex(self.format, &hex[..length])
            .map_err(|_| ObjectReadError::Corrupt("invalid abbreviation width"))?;
        for other in self.prefix_matches(prefix)? {
            if other == id {
                continue;
            }
            let common = hex
                .bytes()
                .zip(other.to_string().bytes())
                .take_while(|(a, b)| a == b)
                .count();
            length = length.max(common + 1);
        }
        ObjectIdPrefix::from_hex(self.format, &hex[..length])
            .map_err(|_| ObjectReadError::Corrupt("invalid abbreviation width"))
    }

    fn require_prefix_format(&self, prefix: ObjectIdPrefix) -> Result<(), ObjectReadError> {
        if prefix.object_format() != self.format {
            return Err(crate::Error::from(ObjectFormatError {
                expected: self.format,
                actual: prefix.object_format(),
            })
            .into());
        }
        Ok(())
    }

    /// Computes Git's automatic minimum abbreviation length from the packed object count.
    ///
    /// The estimate counts entries in each pinned pack, including duplicate copies and alternates.
    /// Loose objects do not increase this estimate. The minimum is seven; each additional digit
    /// provides four bits of space for the birthday-bound estimate. Call once per output operation,
    /// then pass the result to [`Self::abbreviate`] to lengthen actual collisions.
    pub fn auto_abbrev_len(&self) -> usize {
        let count = self
            .stores
            .iter()
            .flat_map(|s| &s.packs)
            .fold(0usize, |count, pack| {
                count.saturating_add(pack.object_count())
            });
        auto_length(count)
    }
}

fn auto_length(count: usize) -> usize {
    let bits = usize::BITS - count.leading_zeros();
    (bits as usize).div_ceil(2).max(7)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_width_boundaries() {
        for (count, width) in [
            (0, 7),
            (16383, 7),
            (16384, 8),
            (65535, 8),
            (65536, 9),
            (262144, 10),
        ] {
            assert_eq!(auto_length(count), width);
        }
    }
    #[test]
    fn ambiguity_lists_candidates_and_abbreviation_extends_half_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::Repository::init(
            crate::ObjectFormat::Sha1,
            dir.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let a: ObjectId = "abcd100000000000000000000000000000000000".parse().unwrap();
        let b: ObjectId = "abcd200000000000000000000000000000000000".parse().unwrap();
        let bucket = repo.object_dir().join("ab");
        fs::create_dir(&bucket).unwrap();
        // Prefix lookup deliberately inspects names rather than verifying payloads.
        for id in [a, b] {
            fs::write(bucket.join(&id.to_string()[2..]), b"").unwrap();
        }
        let objects = repo.objects(crate::PackLimits::default()).unwrap();
        let prefix = ObjectIdPrefix::from_hex(crate::ObjectFormat::Sha1, "abcd").unwrap();
        match objects.resolve_prefix(prefix).unwrap_err() {
            ResolvePrefixError::Ambiguous { candidates, .. } => assert_eq!(candidates, vec![a, b]),
            error => panic!("{error}"),
        }
        assert_eq!(objects.abbreviate(a, 0).unwrap().to_string(), "abcd1");
        assert_eq!(
            objects.abbreviate(a, usize::MAX).unwrap().to_string(),
            a.to_string()
        );
        assert_eq!(
            objects
                .resolve_prefix(objects.abbreviate(a, 4).unwrap())
                .unwrap(),
            a
        );
    }
}
