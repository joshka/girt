use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use super::similarity::{Budget, Content, spend};
use super::{Copies, Error, Kind, Limits, Options, Rewrite};
use crate::{EntryMode, ObjectId, ObjectKind, Objects, TreeChange, TreeValue};

impl Objects {
    /// Infers renames and optional copies between two stored trees, without changing storage.
    ///
    /// Only additions of regular/executable blobs are destinations. Deleted blobs are rename
    /// sources; [`Copies::Modified`] additionally uses preimages of modified blobs, including
    /// mode-only changes, and permits source reuse. Symlinks, gitlinks and directories are never
    /// results. File/directory replacements use the structural comparison's leaf changes.
    /// Both object formats are supported; candidate blobs are read and verified even for exact IDs.
    ///
    /// Exact identity matches precede edited-content inference. Exact ties prefer an unused
    /// deleted source, then equal basename, then source raw path order. Edited ties prefer higher
    /// rounded percentage, equal basename, source path, then destination path; selected pairs
    /// greedily consume destinations and (without copies) sources. This explicit deterministic
    /// policy does not promise Git's every heuristic pairing choice. [`Rewrite`] defines scoring.
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
    /// # Errors
    ///
    /// Returns [`Error`] on invalid options, tree/blob storage failures, missing/wrong-kind blobs,
    /// cancellation, or exhausted candidate/resource limits. Errors return no partial inference;
    /// no repository state is changed. Exact matching is attempted before the candidate limit,
    /// but storage and work bounds apply throughout. See [`Limits`] for memory accounting.
    pub fn detect_rewrites(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        options: Options,
        limits: Limits,
        targets: Option<&[&[u8]]>,
        cancel: &AtomicBool,
    ) -> Result<Vec<Rewrite>, Error> {
        let mut budget = Budget {
            remaining: limits,
            cancel,
        };
        budget.check()?;
        if !(1..=100).contains(&options.similarity) {
            return Err(Error::Similarity(options.similarity));
        }
        let changes = self.compare_trees(old, new, limits.trees, cancel)?;
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
        if options.similarity < 100 {
            selection.edited(&mut blobs, &mut budget)?;
        }
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
        Ok(records)
    }
}

struct Candidate<'a> {
    path: &'a [u8],
    value: TreeValue,
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
                || (copies == Copies::Modified && change.new.is_some_and(|v| is_file(v.mode))))
        {
            sources.push(Candidate {
                path: &change.path,
                value: old,
                deleted: change.new.is_none(),
            });
        }
        if change.old.is_none()
            && let Some(new) = change.new.filter(|value| is_file(value.mode))
        {
            targets.push(Candidate {
                path: &change.path,
                value: new,
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

fn read(objects: &Objects, id: ObjectId, budget: &mut Budget<'_>) -> Result<Blob, Error> {
    let mut limits = budget.remaining.read;
    limits.max_object_bytes = limits.max_object_bytes.min(budget.remaining.max_blob_bytes);
    let object = objects
        .read(id, limits)
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
        !self.used_sources[source] || self.options.copies == Copies::Modified
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

    fn edited(
        &mut self,
        blobs: &mut BTreeMap<ObjectId, Blob>,
        budget: &mut Budget<'_>,
    ) -> Result<(), Error> {
        let sources: Vec<_> = (0..self.sources.len())
            .filter(|&s| self.available(s) && !blobs[&self.sources[s].value.id].bytes.is_empty())
            .collect();
        let targets: Vec<_> = (0..self.targets.len())
            .filter(|&t| {
                !self.used_targets[t] && !blobs[&self.targets[t].value.id].bytes.is_empty()
            })
            .collect();
        if sources.is_empty() || targets.is_empty() {
            return Ok(());
        }
        if sources.len().max(targets.len()) > self.options.candidate_limit {
            return Err(Error::Candidates {
                sources: sources.len(),
                targets: targets.len(),
                limit: self.options.candidate_limit,
            });
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
            source_id: old.value.id,
            target_id: new.value.id,
            similarity,
            kind,
        });
        self.used_sources[source] = true;
        self.used_targets[target] = true;
    }
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}
