//! Prefix discovery without decoding object payloads.
use std::sync::OnceLock;
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
    #[error("ambiguous object prefix {prefix}: {}", candidate_list(.candidates))]
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

fn candidate_list(ids: &[ObjectId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each bucket is published only after all primary/alternate directories were read successfully.
/// Clones share immutable buckets; refresh replaces the cache rather than mutating shared state.
#[derive(Debug, Default)]
pub(super) struct LooseAbbreviations {
    buckets: OnceLock<Box<[OnceLock<Vec<ObjectId>>; 256]>>,
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
        let text = prefix.to_string();
        let mut ids = self.loose_prefix_matches(&text[..2], Some(prefix))?;
        self.append_pack_matches(prefix, &mut ids)?;
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    fn append_pack_matches(
        &self,
        prefix: ObjectIdPrefix,
        ids: &mut Vec<ObjectId>,
    ) -> Result<(), ObjectReadError> {
        for store in &self.stores {
            for pack in &store.packs {
                ids.extend(pack.prefix_matches(prefix)?);
            }
        }
        Ok(())
    }

    fn loose_prefix_matches(
        &self,
        directory_name: &str,
        prefix: Option<ObjectIdPrefix>,
    ) -> Result<Vec<ObjectId>, ObjectReadError> {
        let mut ids = Vec::new();
        let text = prefix.map(|prefix| prefix.to_string());
        for store in &self.stores {
            let directory = store.directory.join(directory_name);
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(ObjectReadError::Path {
                        path: directory,
                        source,
                    });
                }
            };
            for entry in entries {
                let entry = entry.map_err(|source| ObjectReadError::Path {
                    path: directory.clone(),
                    source,
                })?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.len() != self.format.digest_len() * 2 - 2
                    || text
                        .as_ref()
                        .is_some_and(|text| !name.starts_with(&text[2..]))
                {
                    continue;
                }
                let Ok(id) = ObjectId::from_hex(self.format, &format!("{directory_name}{name}"))
                else {
                    continue;
                };
                if prefix.is_some_and(|prefix| !prefix.matches(id)) {
                    continue;
                }
                let kind = entry.file_type().map_err(|source| ObjectReadError::Path {
                    path: entry.path(),
                    source,
                })?;
                if kind.is_file() {
                    ids.push(id);
                } else if kind.is_symlink() {
                    match fs::metadata(entry.path()) {
                        Ok(metadata) if metadata.is_file() => ids.push(id),
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(source) => {
                            return Err(ObjectReadError::Path {
                                path: entry.path(),
                                source,
                            });
                        }
                    }
                }
            }
        }
        Ok(ids)
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "a first digest byte indexes exactly 256 buckets"
    )]
    fn abbreviation_candidates(
        &self,
        prefix: ObjectIdPrefix,
    ) -> Result<Vec<ObjectId>, ObjectReadError> {
        let buckets = self
            .abbreviations
            .buckets
            .get_or_init(|| Box::new(std::array::from_fn(|_| OnceLock::new())));
        let bucket = &buckets[usize::from(prefix.lower().as_bytes()[0])];
        let mut ids = if let Some(ids) = bucket.get() {
            ids.iter()
                .copied()
                .filter(|id| prefix.matches(*id))
                .collect()
        } else {
            let text = prefix.to_string();
            let all = self.loose_prefix_matches(&text[..2], None)?;
            let matches = all
                .iter()
                .copied()
                .filter(|id| prefix.matches(*id))
                .collect();
            // A concurrent successful scan may win publication; either scan is a valid snapshot.
            let _ = bucket.set(all);
            matches
        };
        self.append_pack_matches(prefix, &mut ids)?;
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
    /// let objects = repo.objects_with(PackLimits::default())?;
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
    /// reader across calls to reuse pack indexes and lazy loose-directory snapshots. Each loose
    /// bucket is scanned on its first abbreviation and retained until [`Self::refresh`]; clones
    /// share these snapshots. Prefix resolution remains live. Concurrent writes can invalidate
    /// uniqueness, so refresh before abbreviating against newly written objects.
    ///
    /// # Errors
    ///
    /// Reports format mismatch or storage inspection failure, as [`Self::prefix_matches`] does.
    pub fn abbreviate(
        &self,
        id: ObjectId,
        min_len: usize,
    ) -> Result<ObjectIdPrefix, ObjectReadError> {
        id.require_format(self.format)
            .map_err(crate::loose::Error::from)?;
        let hex = id.to_string();
        let mut length = min_len.clamp(4, hex.len());
        let prefix = ObjectIdPrefix::from_id(id, length);
        for other in self.abbreviation_candidates(prefix)? {
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
        Ok(ObjectIdPrefix::from_id(id, length))
    }

    fn require_prefix_format(&self, prefix: ObjectIdPrefix) -> Result<(), ObjectReadError> {
        if prefix.object_format() != self.format {
            return Err(crate::loose::Error::from(ObjectFormatError {
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
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        let prefix = ObjectIdPrefix::from_hex(crate::ObjectFormat::Sha1, "abcd").unwrap();
        let error = objects.resolve_prefix(prefix).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&a.to_string()) && message.contains(&b.to_string()));
        match error {
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
    #[test]
    fn abbreviation_snapshot_refreshes_without_freezing_prefix_lookup() {
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
        fs::write(bucket.join(&a.to_string()[2..]), b"").unwrap();
        let mut objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        assert_eq!(objects.abbreviate(a, 4).unwrap().hex_len(), 4);
        let retained = objects.clone();
        fs::write(bucket.join(&b.to_string()[2..]), b"").unwrap();
        let prefix = ObjectIdPrefix::from_hex(crate::ObjectFormat::Sha1, "abcd").unwrap();
        assert!(matches!(
            objects.resolve_prefix(prefix),
            Err(ResolvePrefixError::Ambiguous { .. })
        ));
        assert_eq!(objects.abbreviate(a, 4).unwrap().hex_len(), 4);
        assert!(
            objects
                .refresh(
                    crate::PackLimits::default(),
                    crate::AlternateLimits::default().with_max_stores(0)
                )
                .is_err()
        );
        assert_eq!(objects.abbreviate(a, 4).unwrap().hex_len(), 4);
        objects
            .refresh(
                crate::PackLimits::default(),
                crate::AlternateLimits::default(),
            )
            .unwrap();
        assert_eq!(objects.abbreviate(a, 4).unwrap().hex_len(), 5);
        assert_eq!(retained.abbreviate(a, 4).unwrap().hex_len(), 4);
    }
    #[test]
    fn failed_loose_scan_does_not_publish_partial_abbreviations() {
        let dir = tempfile::tempdir().unwrap();
        let format = crate::ObjectFormat::Sha1;
        let repo = crate::Repository::init(format, dir.path().join("repo"), crate::InitKind::Bare)
            .unwrap();
        let alternate =
            crate::Repository::init(format, dir.path().join("alternate"), crate::InitKind::Bare)
                .unwrap();
        fs::write(
            repo.object_dir().join("info/alternates"),
            format!("{}\n", alternate.object_dir().display()),
        )
        .unwrap();
        let a: ObjectId = "abcd100000000000000000000000000000000000".parse().unwrap();
        let b: ObjectId = "abcd200000000000000000000000000000000000".parse().unwrap();
        fs::create_dir(repo.object_dir().join("ab")).unwrap();
        fs::write(repo.object_dir().join("ab").join(&a.to_string()[2..]), b"").unwrap();
        let broken = alternate.object_dir().join("ab");
        fs::write(&broken, b"").unwrap();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        assert!(objects.abbreviate(a, 4).is_err());
        fs::remove_file(&broken).unwrap();
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join(&b.to_string()[2..]), b"").unwrap();
        assert_eq!(objects.abbreviate(a, 4).unwrap().hex_len(), 5);
    }
}
