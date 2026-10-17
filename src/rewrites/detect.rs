use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use super::similarity::{Budget, Content, spend};
use super::{Copies, Detection, Error, InexactSkipped, Kind, Limits, Options, Rewrite};
use crate::{EntryMode, ObjectId, ObjectKind, Objects, TreeChange, TreeValue};

impl Objects {
    /// Infers renames and optional copies between two stored trees, without changing storage.
    ///
    /// Only additions of regular/executable blobs are destinations. Deleted blobs are rename
    /// sources; [`Copies::Modified`] additionally uses preimages of modified blobs, including
    /// mode-only changes, and permits source reuse. [`Copies::ModifiedPostimage`] scores their
    /// postimages but reports their preimage IDs. Symlinks, gitlinks and directories are never
    /// results. File/directory replacements use the structural comparison's leaf changes.
    /// Both object formats are supported; candidate blobs are read and verified even for exact IDs.
    ///
    /// Exact identity matches precede unique-basename pairing and exhaustive edited inference.
    /// Unique unmatched basenames pair without copies at halfway from the minimum score to 100%.
    /// This can prefer a basename pair over a more similar differently named destination.
    /// Exact ties prefer an unused deleted source, then equal basename, then source raw path order.
    /// Edited ties prefer higher rounded percentage, equal basename, source path, then
    /// destination path; selected pairs greedily consume destinations and (without copies)
    /// sources. This explicit deterministic policy does not promise Git's every heuristic
    /// pairing choice. [`Rewrite`] defines scoring.
    ///
    /// `targets` is an optional set of exact destination byte paths, applied **after** all
    /// inference and classification. It cannot change a tie or convert a copy into a rename.
    /// Results sort by target bytes. `None` allows all; an empty slice returns no records but
    /// still performs validation/inference. Paths need no OS/UTF-8 conversion.
    ///
    /// Cancellation is checked before validation, between reads/candidates, during span scanning,
    /// and before and after sorting. Individual storage reads, allocation, tree parsing and sorting
    /// cannot be interrupted. Callers own worker scheduling and must leave `cancel` set until
    /// return. No attributes, clean/smudge/textconv drivers, index or worktree are consulted: these
    /// are comparisons of stored blob bytes. Callers own configuration and merge/history policy.
    ///
    /// [`Self::detect_rewrites_or_exact`] instead returns the pairs already found when the
    /// [`Options::candidate_limit`] skips exhaustive edited-content detection.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on invalid options, tree/blob storage failures, missing/wrong-kind blobs,
    /// cancellation, or exhausted candidate/resource limits. Errors return no partial inference;
    /// no repository state is changed. Exact matching is attempted before the candidate limit,
    /// but storage and work bounds apply throughout. See [`Limits`] for memory accounting.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::rewrites::{Kind, Limits, Options};
    /// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let tree = |files: &[(&[u8], &[u8])]| -> Result<_, Box<dyn std::error::Error>> {
    ///     let entries = files
    ///         .iter()
    ///         .map(|(name, bytes)| {
    ///             Ok(TreeEntry {
    ///                 name: name.to_vec(),
    ///                 mode: EntryMode::Blob,
    ///                 id: loose.write_blob(bytes)?,
    ///             })
    ///         })
    ///         .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    ///     Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, entries)?)?)
    /// };
    ///
    /// let old = tree(&[(b"before", b"same content\n")])?;
    /// let new = tree(&[(b"after", b"same content\n")])?;
    /// let records = repo.objects_with(Default::default())?.detect_rewrites(
    ///     Some(old),
    ///     Some(new),
    ///     Options::default(),
    ///     Limits::default(),
    ///     None,
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(records[0].kind, Kind::Rename);
    /// assert_eq!(records[0].similarity, 100);
    /// assert_eq!(records[0].source_id, records[0].target_id);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn detect_rewrites(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        options: Options,
        limits: Limits,
        targets: Option<&[&[u8]]>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Rewrite>, Error> {
        let detection =
            self.detect_rewrites_or_exact(old, new, options, limits, targets, cancel)?;
        match detection.inexact_skipped {
            None => Ok(detection.rewrites),
            Some(skipped) => Err(Error::Candidates {
                sources: skipped.sources,
                targets: skipped.targets,
                limit: skipped.limit,
            }),
        }
    }

    /// Infers rewrites as [`Self::detect_rewrites`] does, keeping pre-limit matches.
    ///
    /// When the remaining candidates exceed [`Options::candidate_limit`], edited-content
    /// detection is skipped and [`Detection::inexact_skipped`] says so. Exact and unique-basename
    /// pairs are still returned. Callers decide whether to warn, retry with a higher limit, or
    /// accept the pairs found before the limit.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on the same conditions as [`Self::detect_rewrites`], except that an
    /// exceeded candidate limit is reported in the result rather than as [`Error::Candidates`].
    ///
    /// # Keep exact renames when edited detection is too expensive
    ///
    /// The fallback API, available since 0.10.0, keeps a useful result for large changes while
    /// making its limitation visible. Unpaired additions/deletions must remain visible to the
    /// caller. Other work limits and storage failures still return errors, never partial
    /// detection.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::rewrites::{Error, Limits, Options};
    /// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let tree = |files: &[(&[u8], &[u8])]| -> Result<_, Box<dyn std::error::Error>> {
    ///     let entries = files
    ///         .iter()
    ///         .map(|(name, bytes)| {
    ///             Ok(TreeEntry {
    ///                 name: name.to_vec(),
    ///                 mode: EntryMode::Blob,
    ///                 id: loose.write_blob(bytes)?,
    ///             })
    ///         })
    ///         .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    ///     Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, entries)?)?)
    /// };
    ///
    /// let old = tree(&[
    ///     (b"exact-old", b"identical\n"),
    ///     (b"edited-old", b"line one\nline two\n"),
    /// ])?;
    /// let new = tree(&[
    ///     (b"exact-new", b"identical\n"),
    ///     (b"edited-new", b"line one\nline THREE\n"),
    /// ])?;
    /// let objects = repo.objects_with(Default::default())?;
    /// let options = Options::default().with_candidate_limit(0);
    /// let cancel = AtomicBool::new(false);
    /// let detection = objects.detect_rewrites_or_exact(
    ///     Some(old),
    ///     Some(new),
    ///     options,
    ///     Limits::default(),
    ///     None,
    ///     &cancel,
    /// )?;
    /// assert_eq!(detection.rewrites.len(), 1); // The identical pair survived.
    /// assert_eq!(detection.rewrites[0].source, b"exact-old");
    /// let skipped = detection.inexact_skipped.expect("one edited pair remains");
    /// assert_eq!((skipped.sources, skipped.targets, skipped.limit), (1, 1, 0));
    /// assert_eq!(skipped.suggested_limit(), 1);
    /// assert!(matches!(
    ///     objects.detect_rewrites(
    ///         Some(old),
    ///         Some(new),
    ///         options,
    ///         Limits::default(),
    ///         None,
    ///         &cancel
    ///     ),
    ///     Err(Error::Candidates { .. })
    /// ));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn detect_rewrites_or_exact(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        options: Options,
        limits: Limits,
        targets: Option<&[&[u8]]>,
        cancel: &AtomicBool,
    ) -> Result<Detection, Error> {
        let mut budget = Budget {
            remaining: limits,
            cancel,
        };
        budget.check()?;
        if !(1..=100).contains(&options.similarity) {
            return Err(Error::Similarity(options.similarity));
        }
        let changes = self.compare_trees_with(old, new, limits.trees, cancel)?;
        let (sources, destinations) = candidates(&changes, options.copies, &mut budget)?;
        let mut blobs = BTreeMap::new();
        for candidate in sources.iter().chain(&destinations) {
            budget.check()?;
            if let std::collections::btree_map::Entry::Vacant(entry) =
                blobs.entry(candidate.value.id)
            {
                entry.insert(read(self, candidate.value.id, &mut budget)?);
            }
        }
        let mut selection = Selection::new(&sources, &destinations, options);
        selection.exact(&blobs, &mut budget)?;
        let inexact_skipped = if options.similarity < 100 {
            selection.edited(&mut blobs, &mut budget)?
        } else {
            None
        };
        budget.check()?;
        let mut records = selection.records;
        if let Some(targets) = targets {
            // Build once so target filtering cannot multiply candidate work by filter count.
            let mut selected = std::collections::BTreeSet::new();
            for target in targets {
                budget.work(
                    target
                        .len()
                        .saturating_mul((selected.len() + 1).ilog2() as usize + 1),
                )?;
                selected.insert(*target);
            }
            records.retain(|record| selected.contains(record.target.as_slice()));
        }
        records.sort_by(|a, b| a.target.cmp(&b.target));
        budget.check()?;
        Ok(Detection {
            rewrites: records,
            inexact_skipped,
        })
    }
}

struct Candidate<'a> {
    path: &'a [u8],
    value: TreeValue,
    source_id: ObjectId,
    deleted: bool,
}

fn candidates<'a>(
    changes: &'a [TreeChange],
    copies: Copies,
    budget: &mut Budget<'_>,
) -> Result<(Vec<Candidate<'a>>, Vec<Candidate<'a>>), Error> {
    let mut sources = Vec::new();
    let mut targets = Vec::new();
    for change in changes {
        budget.work(1)?;
        if let Some(old) = change.old.filter(|value| is_file(value.mode))
            && (change.new.is_none()
                || (copies != Copies::Disabled && change.new.is_some_and(|v| is_file(v.mode))))
        {
            let value = if copies == Copies::ModifiedPostimage {
                change.new.unwrap_or(old)
            } else {
                old
            };
            sources.push(Candidate {
                path: &change.path,
                value,
                source_id: old.id,
                deleted: change.new.is_none(),
            });
        }
        if change.old.is_none()
            && let Some(new) = change.new.filter(|value| is_file(value.mode))
        {
            targets.push(Candidate {
                path: &change.path,
                value: new,
                source_id: new.id,
                deleted: false,
            });
        }
    }
    Ok((sources, targets))
}

fn is_file(mode: EntryMode) -> bool {
    matches!(mode, EntryMode::Blob | EntryMode::Executable)
}

struct Blob {
    bytes: Vec<u8>,
    content: Option<Content>,
}

impl Blob {
    fn binary(&self) -> bool {
        self.bytes[..self.bytes.len().min(8000)].contains(&0)
    }
}

fn read(objects: &Objects, id: ObjectId, budget: &mut Budget<'_>) -> Result<Blob, Error> {
    let mut limits = budget.remaining.read;
    limits.max_object_bytes = limits.max_object_bytes.min(budget.remaining.max_blob_bytes);
    let object = objects
        .read_with(id, limits)
        .map_err(|source| Error::Read { id, source })?
        .ok_or(Error::Missing(id))?;
    if object.kind() != ObjectKind::Blob {
        return Err(Error::NotBlob {
            id,
            actual: object.kind(),
        });
    }
    let bytes = object.into_data();
    spend(
        &mut budget.remaining.max_blob_bytes,
        bytes.len(),
        "blob bytes",
    )?;
    budget.check()?;
    Ok(Blob {
        bytes,
        content: None,
    })
}

struct Selection<'a> {
    sources: &'a [Candidate<'a>],
    targets: &'a [Candidate<'a>],
    used_sources: Vec<bool>,
    used_targets: Vec<bool>,
    options: Options,
    records: Vec<Rewrite>,
}

impl<'a> Selection<'a> {
    fn new(sources: &'a [Candidate<'a>], targets: &'a [Candidate<'a>], options: Options) -> Self {
        Self {
            sources,
            targets,
            used_sources: vec![false; sources.len()],
            used_targets: vec![false; targets.len()],
            options,
            records: Vec::new(),
        }
    }

    fn available(&self, source: usize) -> bool {
        !self.used_sources[source] || self.options.copies != Copies::Disabled
    }

    fn exact(
        &mut self,
        blobs: &BTreeMap<ObjectId, Blob>,
        budget: &mut Budget<'_>,
    ) -> Result<(), Error> {
        let mut by_id: BTreeMap<ObjectId, Vec<usize>> = BTreeMap::new();
        for (index, source) in self.sources.iter().enumerate() {
            budget.work(1)?;
            if self.options.track_empty || !blobs[&source.value.id].bytes.is_empty() {
                by_id.entry(source.value.id).or_default().push(index);
            }
        }
        for (target, destination) in self.targets.iter().enumerate() {
            budget.work(1)?;
            let Some(sources) = by_id.get(&destination.value.id) else {
                continue;
            };
            let mut best = None;
            for &source in sources {
                budget.pair()?;
                if !self.available(source) {
                    continue;
                }
                let candidate = &self.sources[source];
                let rank = (
                    !candidate.deleted || self.used_sources[source],
                    basename(candidate.path) != basename(destination.path),
                    candidate.path,
                );
                if best.is_none_or(|(_, previous)| rank < previous) {
                    best = Some((source, rank));
                }
            }
            if let Some((source, _)) = best {
                self.select(source, target, 100);
            }
        }
        Ok(())
    }

    /// Pairs remaining candidates by edited content, unless the candidate limit skips it.
    fn edited(
        &mut self,
        blobs: &mut BTreeMap<ObjectId, Blob>,
        budget: &mut Budget<'_>,
    ) -> Result<Option<InexactSkipped>, Error> {
        self.same_basename(blobs, budget)?;
        // Git counts available sources (reusable ones too, with copies) and unmatched destinations,
        // including binary files, and empty files when empty renames are enabled. Decide the skip
        // before filtering to scorable files, which can leave nothing to score.
        let counted = |blob: &Blob| self.options.track_empty || !blob.bytes.is_empty();
        let scorable = |blob: &Blob| {
            !blob.bytes.is_empty() && (self.options.approximate_binary || !blob.binary())
        };
        let source_count = (0..self.sources.len())
            .filter(|&s| self.available(s) && counted(&blobs[&self.sources[s].value.id]))
            .count();
        let target_count = (0..self.targets.len())
            .filter(|&t| !self.used_targets[t] && counted(&blobs[&self.targets[t].value.id]))
            .count();
        // Git compares the candidate product with the squared limit, so one source may be scored
        // against many destinations. Widen so the comparison cannot overflow.
        let limit = self.options.candidate_limit as u128;
        if source_count as u128 * target_count as u128 > limit * limit {
            return Ok(Some(InexactSkipped {
                sources: source_count,
                targets: target_count,
                limit: self.options.candidate_limit,
            }));
        }
        let sources: Vec<_> = (0..self.sources.len())
            .filter(|&s| self.available(s) && scorable(&blobs[&self.sources[s].value.id]))
            .collect();
        let targets: Vec<_> = (0..self.targets.len())
            .filter(|&t| !self.used_targets[t] && scorable(&blobs[&self.targets[t].value.id]))
            .collect();
        if sources.is_empty() || targets.is_empty() {
            return Ok(None);
        }
        for id in sources
            .iter()
            .map(|&s| self.sources[s].value.id)
            .chain(targets.iter().map(|&t| self.targets[t].value.id))
        {
            let blob = blobs.get_mut(&id).expect("candidate blob was loaded");
            if blob.content.is_none() {
                blob.content = Some(Content::new(&blob.bytes, budget)?);
            }
        }
        let mut pairs = Vec::new();
        for source in sources {
            for &target in &targets {
                budget.pair()?;
                let old = blobs[&self.sources[source].value.id]
                    .content
                    .as_ref()
                    .unwrap();
                let new = blobs[&self.targets[target].value.id]
                    .content
                    .as_ref()
                    .unwrap();
                if let Some(score) = old.score(new, self.options.similarity, budget)? {
                    pairs.push((source, target, score));
                }
            }
        }
        budget.check()?;
        pairs.sort_by_key(|&(source, target, score)| {
            (
                std::cmp::Reverse(score),
                basename(self.sources[source].path) != basename(self.targets[target].path),
                self.sources[source].path,
                self.targets[target].path,
            )
        });
        budget.check()?;
        for (source, target, score) in pairs {
            budget.work(1)?;
            if self.available(source) && !self.used_targets[target] {
                self.select(source, target, score);
            }
        }
        Ok(None)
    }

    /// Unique unmatched basenames get first refusal at halfway from the requested score to 100%.
    /// Copy detection skips this pass because its sources remain reusable.
    fn same_basename(
        &mut self,
        blobs: &mut BTreeMap<ObjectId, Blob>,
        budget: &mut Budget<'_>,
    ) -> Result<(), Error> {
        if self.options.copies != Copies::Disabled {
            return Ok(());
        }
        let sources = unique_basenames(
            self.sources,
            &self.used_sources,
            blobs,
            self.options.track_empty,
            budget,
        )?;
        let targets = unique_basenames(
            self.targets,
            &self.used_targets,
            blobs,
            self.options.track_empty,
            budget,
        )?;
        for (name, source) in sources {
            budget.work(
                name.len()
                    .saturating_mul((targets.len() + 1).ilog2() as usize + 1),
            )?;
            let (Some(source), Some(&Some(target))) = (source, targets.get(name)) else {
                continue;
            };
            budget.pair()?;
            let source_id = self.sources[source].value.id;
            let target_id = self.targets[target].value.id;
            if [source_id, target_id].iter().any(|id| {
                let blob = &blobs[id];
                blob.bytes.is_empty() || (!self.options.approximate_binary && blob.binary())
            }) {
                continue;
            }
            for id in [source_id, target_id] {
                let blob = blobs.get_mut(&id).expect("candidate blob was loaded");
                if blob.content.is_none() {
                    blob.content = Some(Content::new(&blob.bytes, budget)?);
                }
            }
            let old = blobs[&source_id].content.as_ref().unwrap();
            let new = blobs[&target_id].content.as_ref().unwrap();
            let minimum = 100 + u16::from(self.options.similarity);
            if let Some(score) = old.score_half_percent(new, minimum, budget)? {
                self.select(source, target, score);
            }
        }
        Ok(())
    }

    fn select(&mut self, source: usize, target: usize, similarity: u8) {
        let old = &self.sources[source];
        let new = &self.targets[target];
        let kind = if old.deleted && !self.used_sources[source] {
            Kind::Rename
        } else {
            Kind::Copy
        };
        self.records.push(Rewrite {
            source: old.path.to_vec(),
            target: new.path.to_vec(),
            source_id: old.source_id,
            target_id: new.value.id,
            similarity,
            kind,
        });
        self.used_sources[source] = true;
        self.used_targets[target] = true;
    }
}

/// Ambiguous basenames remain present as `None`, so a later collision cannot become unique again.
fn unique_basenames<'a>(
    candidates: &[Candidate<'a>],
    used: &[bool],
    blobs: &BTreeMap<ObjectId, Blob>,
    track_empty: bool,
    budget: &mut Budget<'_>,
) -> Result<BTreeMap<&'a [u8], Option<usize>>, Error> {
    let mut names = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        budget.work(
            candidate
                .path
                .len()
                .saturating_mul((names.len() + 1).ilog2() as usize + 1),
        )?;
        if !used[index] && (track_empty || !blobs[&candidate.value.id].bytes.is_empty()) {
            names
                .entry(basename(candidate.path))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
    }
    Ok(names)
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}
