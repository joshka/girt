//! Names for commits relative to the nearest tag, as `git describe` prints them.
//!
//! [`Repository::describe`] finds the tag nearest a commit and how many commits the commit has
//! that the tag does not: `v1.2.0-14-gabc1234` is 14 commits after `v1.2.0`, at commit
//! `abc1234`. Build tools use it for version strings. [`Description::format`] writes that form.
//!
//! ```
//! use girt::describe::DescribeOptions;
//! use girt::refs::{RefEdit, RefName, Reflog};
//! use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r.git"),
//!     InitKind::Bare,
//! )?;
//! let loose = repo.loose_objects();
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let commit = |parents, message: &[u8]| {
//!     loose.write_commit(&Commit::new(CommitFields {
//!         tree,
//!         parents,
//!         author: person.clone(),
//!         committer: person.clone(),
//!         extra_headers: vec![],
//!         message: message.to_vec(),
//!     })?)
//! };
//! let release = commit(vec![], b"release\n")?;
//! let fix = commit(vec![release], b"fix\n")?;
//!
//! // A lightweight tag; `DescribeOptions::new()` alone would consider only annotated tags.
//! let name = RefName::tag("v1.0")?;
//! repo.references()?
//!     .transaction(&[RefEdit::create(name, release, Reflog::Preserve)])?;
//!
//! let objects = repo.objects()?;
//! let options = DescribeOptions::new().with_lightweight_tags();
//! let description = repo
//!     .describe(&objects, fix, &options)?
//!     .expect("a tag is reachable");
//! assert_eq!(description.name, b"v1.0");
//! assert_eq!(description.distance, 1);
//!
//! let abbreviated = objects.abbreviate(fix, 7)?;
//! let text = description.format(Some(&abbreviated), false);
//! assert_eq!(text, format!("v1.0-1-g{abbreviated}").into_bytes());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # How the tag is chosen
//!
//! A tag at the commit itself is used directly. Otherwise girt visits ancestors newest committer
//! date first, as Git does, and collects up to [`DescribeOptions::with_candidates`] tagged commits
//! (10 by default). Of those, the one with the fewest commits between it and the described commit
//! wins; a tie goes to the one found first. When several references name one commit, annotated
//! tags win over lightweight tags, which win over other references; annotated tags with the newest
//! tagger date come first, and remaining ties go to the alphabetically first name.
//!
//! The distance is exact: the number of commits reachable from the described commit and not from
//! the tag, or with [`DescribeOptions::with_first_parent`], the number of first-parent steps. Git
//! stops counting early on commit dates, so with commit dates that run backwards along history
//! Git can report a larger distance than girt.
//!
//! Candidates that are ancestors of an earlier candidate cannot be nearer, so the search stops once
//! every remaining commit is one; on long histories with a recent tag it reads only the commits
//! near the described commit. Counting a distance uses the commit-graph when the repository has
//! one; without it, the complete ancestries are read within [`DescribeOptions::with_limits`].
//!
//! `--dirty`, `--broken` and `--contains` have no equivalent yet.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::AtomicBool;

use crate::refs::{RefName, Target};
use crate::{
    HistoryError, IdMap, IdSet, ObjectId, ObjectIdPrefix, ObjectKind, Objects, PeelError,
    PeelFailure, PeelLimits, ReadCommitError, Repository,
};

/// Which references can name a commit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Candidates {
    /// Annotated tags only, as `git describe` uses by default.
    #[default]
    AnnotatedTags,
    /// Annotated and lightweight tags, as `git describe --tags`.
    Tags,
    /// Every reference under `refs/`, as `git describe --all`. Names keep their category, such as
    /// `tags/v1.0`, `heads/main` or `remotes/origin/main`.
    All,
}

/// How [`Repository::describe`] picks a name.
///
/// [`Self::new`] matches `git describe` without options.
#[derive(Clone, Debug)]
pub struct DescribeOptions {
    candidates: Candidates,
    matches: Vec<Vec<u8>>,
    excludes: Vec<Vec<u8>>,
    max_candidates: usize,
    first_parent: bool,
    limits: crate::HistoryLimits,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl DescribeOptions {
    /// Annotated tags, at most 10 candidates, following every parent.
    pub fn new() -> Self {
        Self {
            candidates: Candidates::AnnotatedTags,
            matches: Vec::new(),
            excludes: Vec::new(),
            max_candidates: 10,
            first_parent: false,
            limits: crate::HistoryLimits::default(),
        }
    }

    /// Bounds the history read while looking for candidates and counting distances.
    ///
    /// Without a commit-graph, counting a distance reads the complete ancestry of the described
    /// commit and of each candidate; raise the bounds for histories larger than
    /// [`HistoryLimits::default`](crate::HistoryLimits::default) allows.
    pub fn with_limits(mut self, limits: crate::HistoryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Chooses which references can name a commit.
    pub fn with_candidates_from(mut self, candidates: Candidates) -> Self {
        self.candidates = candidates;
        self
    }

    /// Also considers lightweight tags, as `git describe --tags`.
    pub fn with_lightweight_tags(self) -> Self {
        self.with_candidates_from(Candidates::Tags)
    }

    /// Considers only names matching the glob `pattern`, as `--match`; several patterns accept a
    /// name matching any of them.
    ///
    /// The name is the tag name without `refs/tags/`, or with [`Candidates::All`] the name without
    /// `refs/tags/`, `refs/heads/` or `refs/remotes/`; other references are then not considered.
    /// `*` and `?` match `/` too, and matching is case-sensitive.
    pub fn with_match(mut self, pattern: impl Into<Vec<u8>>) -> Self {
        self.matches.push(pattern.into());
        self
    }

    /// Skips names matching the glob `pattern`, as `--exclude`; exclusion wins over a match.
    pub fn with_exclude(mut self, pattern: impl Into<Vec<u8>>) -> Self {
        self.excludes.push(pattern.into());
        self
    }

    /// Stops after finding `count` tagged ancestors, as `--candidates`; 10 by default.
    ///
    /// Zero accepts only a tag at the commit itself, as `--exact-match` does.
    pub fn with_candidates(mut self, count: usize) -> Self {
        self.max_candidates = count;
        self
    }

    /// Follows only first parents, as `--first-parent`, counting first-parent steps.
    pub fn with_first_parent(mut self, first_parent: bool) -> Self {
        self.first_parent = first_parent;
        self
    }

    fn patterns_allow(&self, name: &[u8]) -> bool {
        let matches =
            |pattern: &Vec<u8>| crate::wildmatch::matches_without_pathname(pattern, name, 1 << 20);
        if self
            .excludes
            .iter()
            .any(|pattern| matches(pattern) == Some(true))
        {
            return false;
        }
        self.matches.is_empty()
            || self
                .matches
                .iter()
                .any(|pattern| matches(pattern) == Some(true))
    }
}

/// A commit named relative to a reference.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Description {
    /// The name as `git describe` prints it, such as `v1.0`, or `tags/v1.0` with
    /// [`Candidates::All`].
    pub name: Vec<u8>,
    /// The full reference, such as `refs/tags/v1.0`.
    pub reference: RefName,
    /// The commit the reference names, after peeling tags.
    pub tagged: ObjectId,
    /// Commits between the reference and the described commit; zero when they are the same.
    pub distance: usize,
    /// The described commit.
    pub commit: ObjectId,
    /// The annotated tag object names a different tag than its reference, as after renaming the
    /// reference. [`Self::name`] is then the tag object's name, and Git always prints the long
    /// form; `git describe` also warns that the tag "is externally known as" that name.
    pub renamed: bool,
}

impl Description {
    /// Writes the description as `git describe` prints it.
    ///
    /// `abbreviated` is the commit's abbreviated ID, from [`Objects::abbreviate`]; `None` prints
    /// only the name, as `--abbrev=0`. At distance zero only the name is printed unless `long` is
    /// set, as `--long`, which gives `<name>-0-g<id>`.
    ///
    /// A [renamed](Self::renamed) tag always prints the long form, with the full commit ID when
    /// `abbreviated` is `None`, as Git prints it.
    pub fn format(&self, abbreviated: Option<&ObjectIdPrefix>, long: bool) -> Vec<u8> {
        let mut text = self.name.clone();
        let id = match abbreviated {
            Some(abbreviated) => abbreviated.to_string(),
            None if self.renamed => self.commit.to_string(),
            None => return text,
        };
        if long || self.renamed || self.distance != 0 {
            text.extend_from_slice(format!("-{}-g{id}", self.distance).as_bytes());
        }
        text
    }
}

/// Describing a commit failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DescribeError {
    /// References could not be listed.
    #[error(transparent)]
    References(#[from] crate::refs::ReferenceError),
    /// A reference's object could not be read or peeled.
    #[error("peeling {}: {source}", String::from_utf8_lossy(reference.as_bytes()))]
    Peel {
        /// The reference being peeled.
        reference: RefName,
        /// The underlying failure.
        #[source]
        source: Box<PeelError>,
    },
    /// A tag object could not be read.
    #[error(transparent)]
    Tag(#[from] crate::ReadTagError),
    /// A commit could not be read or history could not be walked.
    #[error(transparent)]
    History(#[from] HistoryError),
}

/// A reference that can name its commit, ranked against others at the same commit.
#[derive(Clone, Debug)]
struct Named {
    name: Vec<u8>,
    reference: RefName,
    /// Lower is preferred: 0 annotated tag, 1 lightweight tag, 2 other reference.
    rank: u8,
    tagger_seconds: i64,
    renamed: bool,
}

impl Named {
    fn preferred_over(&self, other: &Self) -> bool {
        (self.rank, Reverse(self.tagger_seconds), &self.name)
            < (other.rank, Reverse(other.tagger_seconds), &other.name)
    }
}

impl Repository {
    /// Names `commit` relative to the nearest reference, as `git describe` does.
    ///
    /// Returns `None` when no reference allowed by `options` names `commit` or one of its
    /// ancestors; `git describe --always` then prints the abbreviated commit ID instead. See the
    /// [module documentation](crate::describe) for how the reference is chosen.
    ///
    /// # Errors
    ///
    /// Returns [`DescribeError`] when references cannot be listed, a candidate reference cannot
    /// be peeled, or history cannot be read within [`crate::HistoryLimits::default`]. References
    /// to trees, blobs or missing objects are skipped, as Git skips them.
    pub fn describe(
        &self,
        objects: &Objects,
        commit: ObjectId,
        options: &DescribeOptions,
    ) -> Result<Option<Description>, DescribeError> {
        let names = candidate_names(self, objects, options)?;
        let describe = |tagged: ObjectId, distance| {
            names.get(&tagged).map(|named| Description {
                name: named.name.clone(),
                reference: named.reference.clone(),
                tagged,
                distance,
                commit,
                renamed: named.renamed,
            })
        };
        if names.contains_key(&commit) {
            return Ok(describe(commit, 0));
        }
        if options.max_candidates == 0 {
            return Ok(None);
        }

        let found = newest_first(objects, commit, options, |id| names.contains_key(&id))?;
        let mut best: Option<(usize, ObjectId)> = None;
        for (tagged, steps) in found {
            let distance = match steps {
                Some(steps) => steps,
                None => objects
                    .walk_range_with(&[commit], &[tagged], options.limits)?
                    .len(),
            };
            if best.is_none_or(|(best, _)| distance < best) {
                best = Some((distance, tagged));
            }
        }
        Ok(best.and_then(|(distance, tagged)| describe(tagged, distance)))
    }
}

/// The preferred reference for each commit that `options` allows.
fn candidate_names(
    repository: &Repository,
    objects: &Objects,
    options: &DescribeOptions,
) -> Result<IdMap<Named>, DescribeError> {
    let references = repository.references()?;
    let listed = match options.candidates {
        Candidates::All => references.list()?,
        Candidates::AnnotatedTags | Candidates::Tags => references.list_prefix("refs/tags/")?,
    };
    let mut names: IdMap<Named> = IdMap::default();
    for reference in listed {
        // A symbolic reference names its target's commit under its own name, as in Git.
        let id = match &reference.target {
            Target::Direct(id) => *id,
            Target::Symbolic(_) => match references.resolve(&reference.name)?.id {
                Some(id) => id,
                None => continue,
            },
        };
        let full = reference.name.as_bytes();
        let Some(name) = display_name(full, options) else {
            continue;
        };
        let peeled = match objects.peel(id, PeelLimits::default(), &AtomicBool::new(false)) {
            Ok(peeled) => peeled,
            Err(error) if matches!(*error.source, PeelFailure::Missing) => continue,
            Err(source) => {
                return Err(DescribeError::Peel {
                    reference: reference.name,
                    source: Box::new(source),
                });
            }
        };
        if peeled.kind != ObjectKind::Commit {
            continue;
        }
        let tag = full.starts_with(b"refs/tags/");
        let annotated = tag && !peeled.tags.is_empty();
        if options.candidates == Candidates::AnnotatedTags && !annotated {
            continue;
        }
        let (tagger_seconds, renamed_to) = if annotated {
            let tag = objects.read_tag(id)?;
            let seconds = tag
                .tagger()
                .ok()
                .flatten()
                .and_then(|tagger| tagger.date().ok())
                .map_or(0, |date| date.seconds);
            let short = &full[b"refs/tags/".len()..];
            let renamed = (tag.name() != short).then(|| tag.name().to_vec());
            (seconds, renamed)
        } else {
            (0, None)
        };
        let renamed = renamed_to.is_some();
        let name = match renamed_to {
            Some(tag_name) if options.candidates == Candidates::All => {
                [b"tags/".as_slice(), &tag_name].concat()
            }
            Some(tag_name) => tag_name,
            None => name,
        };
        let named = Named {
            name,
            reference: reference.name,
            rank: if annotated {
                0
            } else if tag {
                1
            } else {
                2
            },
            tagger_seconds,
            renamed,
        };
        match names.get(&peeled.target) {
            Some(existing) if !named.preferred_over(existing) => {}
            _ => {
                names.insert(peeled.target, named);
            }
        }
    }
    Ok(names)
}

/// The printed name of a reference, or `None` when `options` does not allow it.
fn display_name(full: &[u8], options: &DescribeOptions) -> Option<Vec<u8>> {
    let patterned = !options.matches.is_empty() || !options.excludes.is_empty();
    match options.candidates {
        Candidates::AnnotatedTags | Candidates::Tags => {
            let name = full.strip_prefix(b"refs/tags/")?;
            options.patterns_allow(name).then(|| name.to_vec())
        }
        Candidates::All => {
            let name = full.strip_prefix(b"refs/")?;
            if patterned {
                let short = [&b"refs/tags/"[..], b"refs/heads/", b"refs/remotes/"]
                    .iter()
                    .find_map(|prefix| full.strip_prefix(*prefix))?;
                if !options.patterns_allow(short) {
                    return None;
                }
            }
            Some(name.to_vec())
        }
    }
}

/// Visits ancestors of `start` newest committer date first and returns the first
/// `options.max_candidates` commits for which `is_candidate` holds, in the order found. With
/// first parents only, each also carries its number of first-parent steps from `start`.
///
/// Candidates that are ancestors of an earlier candidate count toward the limit, as in Git, but
/// are not returned: they are farther than that candidate. The walk stops early once every queued
/// commit is such an ancestor, since no later candidate could be nearer.
fn newest_first(
    objects: &Objects,
    start: ObjectId,
    options: &DescribeOptions,
    is_candidate: impl Fn(ObjectId) -> bool,
) -> Result<Vec<(ObjectId, Option<usize>)>, DescribeError> {
    let mut found = Vec::new();
    let mut counted = 0usize;
    let mut seen = IdSet::default();
    // Ancestors of a found candidate, and how many queued commits are not.
    let mut covered = IdSet::default();
    let mut queued = IdSet::default();
    let mut uncovered = 0usize;
    // Equal dates leave the earlier-queued commit first.
    let mut queue = BinaryHeap::new();
    let mut order = 0usize;
    seen.insert(start);
    queued.insert(start);
    uncovered += 1;
    queue.push((date(objects, start)?.0, Reverse(order), start, 0usize));
    while let Some((_, _, id, steps)) = queue.pop() {
        queued.remove(&id);
        let mut is_covered = covered.contains(&id);
        if !is_covered {
            uncovered -= 1;
        }
        if id != start && is_candidate(id) {
            counted += 1;
            if !is_covered {
                found.push((id, options.first_parent.then_some(steps)));
                covered.insert(id);
                is_covered = true;
            }
            if counted == options.max_candidates {
                break;
            }
        }
        if !objects.shallow_roots().contains(id) {
            let parents = date(objects, id)?.1;
            let parents = if options.first_parent {
                &parents[..parents.len().min(1)]
            } else {
                &parents[..]
            };
            for &parent in parents {
                let newly_covered = is_covered && covered.insert(parent);
                if seen.insert(parent) {
                    if seen.len() > options.limits.max_commits {
                        return Err(HistoryError::Limit("commits").into());
                    }
                    order += 1;
                    queued.insert(parent);
                    if !covered.contains(&parent) {
                        uncovered += 1;
                    }
                    queue.push((date(objects, parent)?.0, Reverse(order), parent, steps + 1));
                } else if newly_covered && queued.contains(&parent) {
                    uncovered -= 1;
                }
            }
        }
        if !found.is_empty() && uncovered == 0 {
            break;
        }
    }
    Ok(found)
}

/// A commit's committer timestamp and parents, from the commit-graph when it has the commit.
fn date(objects: &Objects, id: ObjectId) -> Result<(i64, Vec<ObjectId>), HistoryError> {
    if let Some(record) = objects
        .commit_graph
        .as_ref()
        .and_then(|graph| graph.get(id))
    {
        let seconds = i64::try_from(record.commit_time).unwrap_or(i64::MAX);
        return Ok((seconds, record.parents.clone()));
    }
    let commit = objects.read_commit(id).map_err(|error| match error {
        ReadCommitError::Missing(id) => HistoryError::Missing(id),
        ReadCommitError::Kind { id, .. } => HistoryError::NotCommit(id),
        ReadCommitError::Parse(source) => HistoryError::Parse { id, source },
        ReadCommitError::Read(source) => HistoryError::Read { id, source },
    })?;
    let seconds = commit
        .committer()
        .ok()
        .flatten()
        .and_then(|committer| committer.date().ok())
        .map_or(0, |date| date.seconds);
    Ok((seconds, commit.parents().to_vec()))
}
