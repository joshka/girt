//! Recorded conflict resolutions that Git and girt reuse: "rerere".
//!
//! When a merge leaves a file with conflict markers, rerere records the conflict, and once the
//! file is resolved it records the resolution. The next time the same conflict appears, in any
//! merge, cherry-pick, rebase or `am`, the recorded resolution is applied to the file.
//!
//! Storage is Git's, so resolutions recorded by either tool are replayed by the other:
//!
//! - `rr-cache/<id>/preimage` holds the conflicted file in normalised form, `postimage` the
//!   resolved file, and `thisimage` the normalised file a resolution was last replayed onto. A
//!   second conflict with the same ID but different surrounding text is a *variant* stored as
//!   `preimage.1`, `postimage.1` and so on.
//! - `MERGE_RR` lists the conflicts awaiting resolution as `<id>[.<variant>]\t<path>\0`.
//!
//! Normalisation drops the marker labels and any base section (diff3 and zdiff3 styles) and puts
//! the two sides of each conflict in byte order, so the same conflict has the same ID whichever
//! side was ours. The ID hashes each conflict's sides with the repository's object hash; text
//! outside the conflicts does not affect it. See [`normalize`].
//!
//! [`Rerere::run_on_index`] does what `git rerere` does after a conflicted operation or before a
//! commit; call it after [`Repository::checkout_merge`] publishes conflicts.
//! [`is_enabled`] applies `rerere.enabled`, which defaults to on when `rr-cache` exists.
//!
//! ```
//! use girt::ObjectFormat;
//! use girt::rerere::normalize;
//!
//! let ours = b"a\n<<<<<<< HEAD\nMAIN\n=======\nSIDE\n>>>>>>> side\nc\n";
//! let theirs = b"a\n<<<<<<< HEAD\nSIDE\n||||||| base\nb\n=======\nMAIN\n>>>>>>> main\nc\n";
//! let one = normalize(ours, ObjectFormat::Sha1)?.unwrap();
//! let other = normalize(theirs, ObjectFormat::Sha1)?.unwrap();
//!
//! assert_eq!(one.id, other.id);
//! assert_eq!(
//!     one.id.to_string(),
//!     "27d08dca9e6e45868a32cd28625ad61784a53d0e"
//! );
//! assert_eq!(
//!     one.preimage,
//!     b"a\n<<<<<<<\nMAIN\n=======\nSIDE\n>>>>>>>\nc\n"
//! );
//! # Ok::<(), girt::rerere::Error>(())
//! ```
//!
//! # Limitations
//!
//! Markers are seven characters wide; a `conflict-marker-size` attribute is not consulted.
//! Nested conflict markers, as recursive merges of conflicted bases produce, are refused.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{fmt, fs};

use crate::{ObjectFormat, ObjectId, Repository};

const WIDTH: usize = 7;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// A rerere operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A conflict began inside another conflict.
    #[error("nested conflict markers at line {0}")]
    Nested(usize),
    /// A conflict was not closed by an end marker, or a separator was missing.
    #[error("unterminated conflict starting at line {0}")]
    Unterminated(usize),
    /// `MERGE_RR` is malformed.
    #[error("malformed MERGE_RR")]
    MergeRr,
    /// A configuration value is invalid.
    #[error(transparent)]
    Config(#[from] crate::config::InvalidValue),
    /// A configuration value is valid Git but not supported by girt.
    #[error("unsupported {0} value; use a whole number of days")]
    Unsupported(&'static str),
    /// The repository has no working tree.
    #[error("rerere needs a working tree")]
    Bare,
    /// A path is not representable on this platform.
    #[error("path is not representable here: {0:?}")]
    Path(Vec<u8>),
    /// Merging a recorded resolution failed.
    #[error(transparent)]
    Merge(#[from] crate::merge::Error),
    /// Reading or writing the index failed.
    #[error(transparent)]
    Index(#[from] crate::index::StorageError),
    /// Editing the index failed.
    #[error(transparent)]
    IndexEdit(#[from] crate::index::Error),
    /// Writing an object failed.
    #[error(transparent)]
    Object(#[from] crate::loose::Error),
    /// Opening the object store failed.
    #[error(transparent)]
    ObjectRead(#[from] crate::ObjectReadError),
    /// Loading the path conversions for staging failed.
    #[error(transparent)]
    FilterLoad(#[from] crate::attributes::LoadError),
    /// Converting a file for staging failed, including a refused filter command.
    #[error(transparent)]
    Filter(#[from] crate::filter::Error),
    /// Reading a stage's blob failed.
    #[error(transparent)]
    ReadBlob(#[from] crate::ReadBlobError),
    /// A file could not be read or written.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A conflict's identity in `rr-cache`: the hash of its sides, plus a variant number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConflictId {
    /// The hash of the normalised conflict sides.
    pub hash: ObjectId,
    /// Which of the conflicts sharing this hash; 0 is stored without a suffix.
    pub variant: u32,
}

impl fmt::Display for ConflictId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.variant {
            0 => write!(f, "{}", self.hash),
            n => write!(f, "{}.{n}", self.hash),
        }
    }
}

impl ConflictId {
    fn parse(text: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(text).ok()?;
        let (hash, variant) = match text.split_once('.') {
            Some((hash, variant)) => (hash, variant.parse().ok()?),
            None => (text, 0),
        };
        Some(Self {
            hash: hash.parse().ok()?,
            variant,
        })
    }

    fn file(&self, name: &str) -> String {
        match self.variant {
            0 => name.to_owned(),
            n => format!("{name}.{n}"),
        }
    }
}

/// A conflicted file in rerere's normalised form.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Normalized {
    /// The hash identifying the conflict, as `rr-cache` names it.
    pub id: ObjectId,
    /// The file with unlabeled markers, base sections removed and sides in byte order.
    pub preimage: Vec<u8>,
}

/// Normalises a conflicted file and computes its conflict ID, or `None` without conflicts.
///
/// Each `<<<<<<<` … `=======` … `>>>>>>>` region keeps its two sides, without marker labels and
/// without the `|||||||` base section diff3 and zdiff3 add, ordered so the bytewise smaller side
/// comes first. The ID is the hash, in `format`, of every conflict's sides in that order, each
/// followed by a NUL byte. Text outside conflicts is copied unchanged and not hashed.
///
/// # Errors
///
/// Returns [`Error::Nested`] for a conflict inside another and [`Error::Unterminated`] for one
/// that does not end.
pub fn normalize(content: &[u8], format: ObjectFormat) -> Result<Option<Normalized>, Error> {
    #[derive(PartialEq)]
    enum Part {
        Outside,
        First,
        Base,
        Second,
    }

    let mut preimage = Vec::with_capacity(content.len());
    let mut hashed = Vec::new();
    let mut conflicts = 0;
    let mut part = Part::Outside;
    let (mut first, mut second) = (Vec::new(), Vec::new());
    let mut start = 0;

    for (number, line) in lines(content).enumerate() {
        let number = number + 1;
        match part {
            Part::Outside if is_marker(line, b'<') => {
                part = Part::First;
                start = number;
            }
            Part::Outside => preimage.extend_from_slice(line),
            _ if is_marker(line, b'<') => return Err(Error::Nested(number)),
            Part::First if is_marker(line, b'|') => part = Part::Base,
            Part::First | Part::Base if is_separator(line) => part = Part::Second,
            Part::First => first.extend_from_slice(line),
            Part::Base => {}
            Part::Second if is_marker(line, b'>') => {
                let (low, high) = if first <= second {
                    (&first, &second)
                } else {
                    (&second, &first)
                };
                for side in [low, high] {
                    hashed.extend_from_slice(side);
                    hashed.push(0);
                }
                preimage.extend_from_slice(&[b'<'; WIDTH]);
                preimage.push(b'\n');
                preimage.extend_from_slice(low);
                preimage.extend_from_slice(&[b'='; WIDTH]);
                preimage.push(b'\n');
                preimage.extend_from_slice(high);
                preimage.extend_from_slice(&[b'>'; WIDTH]);
                preimage.push(b'\n');
                first.clear();
                second.clear();
                conflicts += 1;
                part = Part::Outside;
            }
            Part::Second => second.extend_from_slice(line),
        }
    }
    if part != Part::Outside {
        return Err(Error::Unterminated(start));
    }
    if conflicts == 0 {
        return Ok(None);
    }

    Ok(Some(Normalized {
        id: format.checksum(&hashed),
        preimage,
    }))
}

/// Lines including their `\n`; the last may lack one.
fn lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    content
        .split_inclusive(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
}

/// A conflict marker: seven `c` characters, then what Git requires after them.
///
/// As observed with `git rerere`, the opening `<<<<<<<` and closing `>>>>>>>` markers need a
/// space (their label follows), while `|||||||` and `=======` may also end the line.
fn is_marker(line: &[u8], c: u8) -> bool {
    if line.len() < WIDTH || !line[..WIDTH].iter().all(|&b| b == c) {
        return false;
    }
    match c {
        b'<' | b'>' => line.get(WIDTH) == Some(&b' '),
        _ => matches!(line.get(WIDTH), None | Some(b' ' | b'\n' | b'\r')),
    }
}

fn is_separator(line: &[u8]) -> bool {
    is_marker(line, b'=')
}

/// Whether rerere is active: `rerere.enabled`, or when unset, whether `rr-cache` exists.
///
/// # Errors
///
/// Returns [`Error::Config`] for a value that is not a boolean.
pub fn is_enabled(repo: &Repository) -> Result<bool, Error> {
    match repo.config().boolean("rerere", None, "enabled")? {
        Some(enabled) => Ok(enabled),
        None => Ok(repo.common_dir().join("rr-cache").is_dir()),
    }
}

/// A conflict `MERGE_RR` tracks: its ID and the path it was found in.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Tracked {
    /// The conflict's `rr-cache` identity.
    pub id: ConflictId,
    /// The repository-relative path.
    pub path: Vec<u8>,
}

/// What [`Rerere::run`] did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Report {
    /// Paths whose conflict was recorded ("Recorded preimage").
    pub recorded_conflicts: Vec<Vec<u8>>,
    /// Paths resolved by replaying a recorded resolution ("Resolved … using previous
    /// resolution").
    pub replayed: Vec<Vec<u8>>,
    /// Paths staged after replay because `rerere.autoUpdate` is set.
    pub staged: Vec<Vec<u8>>,
    /// Paths whose resolution was recorded ("Recorded resolution").
    pub recorded_resolutions: Vec<Vec<u8>>,
}

/// The two files `git rerere diff` compares, from [`Rerere::diff`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Diff {
    /// The recorded conflict, normalised.
    pub preimage: Vec<u8>,
    /// The file in the working tree now.
    pub current: Vec<u8>,
}

/// What [`gc`] removed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct GcReport {
    /// Recorded conflicts removed, by variant.
    pub removed: Vec<ConflictId>,
}

/// Recorded resolutions for one repository and the conflicts awaiting resolution.
///
/// Callers must hold exclusive access to the working tree, the index and `MERGE_RR`.
#[derive(Debug)]
pub struct Rerere<'repo> {
    repo: &'repo Repository,
    auto_update: bool,
    tracked: Vec<Tracked>,
}

impl<'repo> Rerere<'repo> {
    /// Reads `MERGE_RR` and `rerere.autoUpdate`. Use [`is_enabled`] to decide whether to run.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bare`] without a working tree, [`Error::MergeRr`] for a malformed
    /// `MERGE_RR`, and configuration and I/O errors.
    pub fn open(repo: &'repo Repository) -> Result<Self, Error> {
        if repo.worktree().is_none() {
            return Err(Error::Bare);
        }
        let auto_update = repo
            .config()
            .boolean("rerere", None, "autoupdate")?
            .unwrap_or(false);
        let tracked = match fs::read(repo.git_dir().join("MERGE_RR")) {
            Ok(bytes) => parse_merge_rr(&bytes).ok_or(Error::MergeRr)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            repo,
            auto_update,
            tracked,
        })
    }

    /// Whether replayed resolutions are staged (`rerere.autoUpdate`).
    pub fn auto_update(&self) -> bool {
        self.auto_update
    }

    /// The conflicts awaiting resolution, as `MERGE_RR` lists them.
    pub fn tracked(&self) -> &[Tracked] {
        &self.tracked
    }

    /// Does what `git rerere` does.
    ///
    /// First, every tracked path whose file no longer has conflict markers has its resolution
    /// recorded as the `postimage` and stops being tracked. Then each path in `conflicted` (the
    /// index's unmerged paths) that is not yet tracked is normalised. When a recorded resolution
    /// of the same conflict merges cleanly onto it, the file is rewritten with the resolution,
    /// and staged when `rerere.autoUpdate` is set. Otherwise its conflict is recorded as a
    /// `preimage` and tracked. Files without conflict markers are left alone.
    ///
    /// # Errors
    ///
    /// Returns normalisation errors for malformed markers and I/O, index and merge errors.
    /// `MERGE_RR` is written after every path has been handled.
    pub fn run(&mut self, conflicted: &[Vec<u8>]) -> Result<Report, Error> {
        let mut report = Report::default();
        let format = self.repo.object_format();

        let mut still = Vec::new();
        for tracked in std::mem::take(&mut self.tracked) {
            let Some(content) = self.read_regular(&tracked.path)? else {
                continue;
            };
            if let Some(normalized) = normalize(&content, format)? {
                // A resolution recorded since this conflict was tracked can resolve it now.
                if let Some((id, resolved)) = self.replay(&normalized)? {
                    self.apply_replay(id, &tracked.path, &normalized, &resolved, &mut report)?;
                } else {
                    still.push(tracked);
                }
                continue;
            }
            self.write_entry(tracked.id, "postimage", &content)?;
            report.recorded_resolutions.push(tracked.path);
        }
        self.tracked = still;

        for path in conflicted {
            if self.tracked.iter().any(|t| &t.path == path) {
                continue;
            }
            let Some(content) = self.read_regular(path)? else {
                continue;
            };
            let Some(normalized) = normalize(&content, format)? else {
                continue;
            };

            if let Some((id, resolved)) = self.replay(&normalized)? {
                self.apply_replay(id, path, &normalized, &resolved, &mut report)?;
                continue;
            }

            let id = self.variant_for(&normalized)?;
            self.write_entry(id, "preimage", &normalized.preimage)?;
            self.tracked.push(Tracked {
                id,
                path: path.clone(),
            });
            report.recorded_conflicts.push(path.clone());
        }

        self.write_merge_rr()?;
        Ok(report)
    }

    fn apply_replay(
        &self,
        id: ConflictId,
        path: &[u8],
        normalized: &Normalized,
        resolved: &[u8],
        report: &mut Report,
    ) -> Result<(), Error> {
        fs::write(self.worktree_path(path)?, resolved)?;
        self.write_entry(id, "thisimage", &normalized.preimage)?;
        report.replayed.push(path.to_vec());
        if self.auto_update {
            self.stage(path, resolved)?;
            report.staged.push(path.to_vec());
        }
        Ok(())
    }

    /// Runs [`Self::run`] on the index's unmerged paths, as `git rerere` does.
    ///
    /// Call this after publishing conflicts with [`Repository::checkout_merge`] (for a merge,
    /// cherry-pick, revert, rebase step or stash apply) to replay known resolutions, and again
    /// before committing the resolved result to record the new resolutions. Check
    /// [`is_enabled`] first, as Git only does this when rerere is enabled.
    ///
    /// # Errors
    ///
    /// Returns index read errors and the errors of [`Self::run`].
    pub fn run_on_index(&mut self) -> Result<Report, Error> {
        let index = self.repo.read_index(Default::default())?;
        let mut unmerged: Vec<Vec<u8>> = Vec::new();
        for path in unmerged_files(index.as_ref()) {
            unmerged.push(path);
        }
        self.run(&unmerged)
    }
    /// Tracked paths, as `git rerere status` lists them.
    pub fn status(&self) -> impl Iterator<Item = &[u8]> {
        self.tracked.iter().map(|t| t.path.as_slice())
    }

    /// Tracked paths whose files still have conflict markers, as `git rerere remaining` lists.
    ///
    /// # Errors
    ///
    /// Returns I/O and normalisation errors.
    pub fn remaining(&self) -> Result<Vec<Vec<u8>>, Error> {
        let mut remaining = Vec::new();
        for tracked in &self.tracked {
            if let Some(content) = self.read_regular(&tracked.path)?
                && normalize(&content, self.repo.object_format())?.is_some()
            {
                remaining.push(tracked.path.clone());
            }
        }
        Ok(remaining)
    }

    /// The recorded preimage and current file of a tracked path, which `git rerere diff`
    /// compares.
    ///
    /// # Errors
    ///
    /// Returns I/O errors; `None` when `path` is not tracked.
    pub fn diff(&self, path: &[u8]) -> Result<Option<Diff>, Error> {
        let Some(tracked) = self.tracked.iter().find(|t| t.path == path) else {
            return Ok(None);
        };
        let preimage = fs::read(self.entry_file(tracked.id, "preimage"))?;
        let current = fs::read(self.worktree_path(path)?)?;
        Ok(Some(Diff { preimage, current }))
    }

    /// Forgets the recorded resolution of each unmerged path in `paths`, as
    /// `git rerere forget` does, and returns the paths whose resolution was removed.
    ///
    /// The conflict is rebuilt from the index's base, ours and theirs stages, its resolution
    /// (`postimage`) is removed, its preimage rewritten, and the path tracked again so the next
    /// resolution is recorded.
    ///
    /// # Errors
    ///
    /// Returns index, object, merge and I/O errors.
    pub fn forget(&mut self, paths: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Error> {
        use crate::index::Stage;

        let Some(index) = self.repo.read_index(Default::default())? else {
            return Ok(Vec::new());
        };
        let objects = self.repo.objects()?;
        let mut forgotten = Vec::new();
        for path in paths {
            let stage = |wanted: Stage| -> Result<Option<Vec<u8>>, Error> {
                let Some(entry) = index
                    .entries()
                    .iter()
                    .find(|e| &e.path == path && e.stage == wanted)
                else {
                    return Ok(None);
                };
                Ok(Some(objects.read_blob(entry.id)?))
            };
            let (Some(ours), Some(theirs)) = (stage(Stage::Ours)?, stage(Stage::Theirs)?) else {
                continue;
            };
            let base = stage(Stage::Base)?.unwrap_or_default();
            let merged = crate::merge::content(&base, &ours, &theirs)?;
            let text = merged.render(&crate::merge::MarkerOptions::default());
            let Some(normalized) = normalize(&text, self.repo.object_format())? else {
                continue;
            };

            for id in self.variants(normalized.id)? {
                if fs::read(self.entry_file(id, "preimage")).ok().as_deref()
                    != Some(normalized.preimage.as_slice())
                {
                    continue;
                }
                remove_if_present(&self.entry_file(id, "postimage"))?;
                self.write_entry(id, "preimage", &normalized.preimage)?;
                self.tracked.retain(|t| &t.path != path);
                self.tracked.push(Tracked {
                    id,
                    path: path.clone(),
                });
                forgotten.push(path.clone());
            }
        }
        self.write_merge_rr()?;
        Ok(forgotten)
    }

    /// Forgets the tracked conflicts, as `git rerere clear` does when an operation is aborted.
    ///
    /// Recorded conflicts without a resolution are removed from `rr-cache`; resolutions are kept.
    ///
    /// # Errors
    ///
    /// Returns I/O errors.
    pub fn clear(&mut self) -> Result<(), Error> {
        for tracked in std::mem::take(&mut self.tracked) {
            if !self.entry_file(tracked.id, "postimage").exists() {
                for name in ["preimage", "thisimage"] {
                    remove_if_present(&self.entry_file(tracked.id, name))?;
                }
                let directory = self.cache().join(tracked.id.hash.to_string());
                let _ = fs::remove_dir(directory);
            }
        }
        self.write_merge_rr()
    }

    /// The first recorded resolution of this conflict that merges cleanly onto it.
    fn replay(&self, normalized: &Normalized) -> Result<Option<(ConflictId, Vec<u8>)>, Error> {
        for id in self.variants(normalized.id)? {
            let (Ok(preimage), Ok(postimage)) = (
                fs::read(self.entry_file(id, "preimage")),
                fs::read(self.entry_file(id, "postimage")),
            ) else {
                continue;
            };
            let merged = crate::merge::content(&preimage, &normalized.preimage, &postimage)?;
            if !merged.is_clean() {
                continue;
            }
            // Rewriting the postimage marks the resolution as used for `gc`.
            self.write_entry(id, "postimage", &postimage)?;
            let resolved = merged.render(&crate::merge::MarkerOptions::default());
            return Ok(Some((id, resolved)));
        }
        Ok(None)
    }

    /// The variant to record this conflict under: one with the same preimage and no
    /// resolution, else the lowest unused number.
    fn variant_for(&self, normalized: &Normalized) -> Result<ConflictId, Error> {
        let existing = self.variants(normalized.id)?;
        for &id in &existing {
            let same = fs::read(self.entry_file(id, "preimage")).ok().as_deref()
                == Some(normalized.preimage.as_slice());
            if same && !self.entry_file(id, "postimage").exists() {
                return Ok(id);
            }
        }
        let variant = (0..)
            .find(|n| !existing.iter().any(|id| id.variant == *n))
            .unwrap_or(0);
        Ok(ConflictId {
            hash: normalized.id,
            variant,
        })
    }

    /// The recorded variants of a conflict, in variant order.
    fn variants(&self, hash: ObjectId) -> Result<Vec<ConflictId>, Error> {
        let directory = self.cache().join(hash.to_string());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut variants = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let variant = match name.strip_prefix("preimage") {
                Some("") => 0,
                Some(suffix) => match suffix.strip_prefix('.').and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => continue,
                },
                None => continue,
            };
            variants.push(ConflictId { hash, variant });
        }
        variants.sort_by_key(|id| id.variant);
        Ok(variants)
    }

    fn cache(&self) -> PathBuf {
        self.repo.common_dir().join("rr-cache")
    }

    fn entry_file(&self, id: ConflictId, name: &str) -> PathBuf {
        self.cache().join(id.hash.to_string()).join(id.file(name))
    }

    fn write_entry(&self, id: ConflictId, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let path = self.entry_file(id, name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomically(&path, bytes)?;
        Ok(())
    }

    /// Reads a working file, or `None` when it is missing or not a regular file.
    ///
    /// Symbolic links are never followed, as Git leaves conflicted links to the user; following
    /// one could read or rewrite a file outside the working tree.
    fn read_regular(&self, path: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let path = self.worktree_path(path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(fs::read(path)?)),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn worktree_path(&self, path: &[u8]) -> Result<PathBuf, Error> {
        let worktree = self.repo.worktree().ok_or(Error::Bare)?;
        let relative =
            crate::source_files::os_path(path).ok_or_else(|| Error::Path(path.to_vec()))?;
        Ok(worktree.join(relative))
    }

    /// Replaces a path's conflict stages with a stage-0 entry for `content`.
    ///
    /// The working file is converted as `git add` would (line endings, encoding, `ident`) before
    /// it is stored. Filter drivers that would run a command are refused.
    fn stage(&self, path: &[u8], content: &[u8]) -> Result<(), Error> {
        use crate::filter::{Commands, Staged};
        use crate::index::{Entry, Mode, Stage};

        let mut edit = self.repo.edit_index(Default::default())?;
        let mut mode = Mode::Regular;
        let mut ours = None;
        let mut entries = Vec::with_capacity(edit.index().entries().len());
        for entry in edit.index().entries() {
            if entry.path != path {
                entries.push(entry.clone());
            } else if entry.stage == Stage::Ours {
                mode = entry.mode;
                ours = Some(entry.id);
            }
        }

        let objects = self.repo.objects()?;
        let mut filters = self
            .repo
            .filters(&crate::workflow::Environment::default(), Commands::Refuse)?;
        let staged = ours.map_or(Staged::Absent, |id| Staged::Object(&objects, id));
        let stored = filters.to_repository(path, content, staged)?;
        let id = self.repo.loose_objects().write_blob(&stored.bytes)?;
        let position = entries
            .binary_search_by(|e| e.path.as_slice().cmp(path))
            .unwrap_or_else(|p| p);
        entries.insert(position, Entry::new(path.to_vec(), mode, id));
        edit.replace_entries(entries)?;
        edit.commit()?;
        Ok(())
    }

    fn write_merge_rr(&self) -> Result<(), Error> {
        let path = self.repo.git_dir().join("MERGE_RR");
        if self.tracked.is_empty() {
            return remove_if_present(&path);
        }
        let mut bytes = Vec::new();
        for tracked in &self.tracked {
            bytes.extend_from_slice(tracked.id.to_string().as_bytes());
            bytes.push(b'\t');
            bytes.extend_from_slice(&tracked.path);
            bytes.push(0);
        }
        write_atomically(&path, &bytes)?;
        Ok(())
    }
}

/// Unmerged paths rerere can handle: both sides are staged as regular files.
///
/// Git leaves add/delete, symbolic link and submodule conflicts to the user.
fn unmerged_files(index: Option<&crate::index::Index>) -> Vec<Vec<u8>> {
    use crate::index::{Mode, Stage};

    let mut paths: Vec<Vec<u8>> = Vec::new();
    let entries = index.map(|index| index.entries()).unwrap_or_default();
    for entry in entries {
        let regular = matches!(entry.mode, Mode::Regular | Mode::Executable);
        if entry.stage != Stage::Theirs || !regular {
            continue;
        }
        let ours = entries.iter().any(|other| {
            other.path == entry.path
                && other.stage == Stage::Ours
                && matches!(other.mode, Mode::Regular | Mode::Executable)
        });
        if ours {
            paths.push(entry.path.clone());
        }
    }
    paths
}

/// Unmerged paths in the index that rerere does not handle, which `git rerere remaining` also
/// lists.
///
/// # Errors
///
/// Returns index read errors.
pub fn unhandled(repo: &Repository) -> Result<Vec<Vec<u8>>, Error> {
    let index = repo.read_index(Default::default())?;
    let handled = unmerged_files(index.as_ref());
    let mut paths: Vec<Vec<u8>> = Vec::new();
    for entry in index.iter().flat_map(|index| index.entries()) {
        let unmerged = entry.stage != crate::index::Stage::Normal;
        if unmerged && !handled.contains(&entry.path) && paths.last() != Some(&entry.path) {
            paths.push(entry.path.clone());
        }
    }
    Ok(paths)
}

/// Removes old `rr-cache` entries, as `git rerere gc` does, with the configured ages.
///
/// `gc.rerereResolved` (default 60 days) applies to entries with a resolution, measured from
/// the `postimage`'s modification time, which replaying refreshes. `gc.rerereUnresolved`
/// (default 15 days) applies to entries without one, measured from the `preimage`.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for ages that are not whole numbers of days, and I/O errors.
pub fn gc(repo: &Repository, now: SystemTime) -> Result<GcReport, Error> {
    let days = |name: &'static str, default: u64| -> Result<Duration, Error> {
        match repo.config().integer("gc", None, name) {
            Ok(Some(days)) => Ok(DAY * u32::try_from(days.max(0)).unwrap_or(u32::MAX)),
            Ok(None) => Ok(DAY * u32::try_from(default).unwrap_or(u32::MAX)),
            Err(_) => Err(Error::Unsupported(name)),
        }
    };
    gc_with(
        repo,
        now,
        days("rerereresolved", 60)?,
        days("rerereunresolved", 15)?,
    )
}

/// [`gc`] with explicit ages instead of configuration.
///
/// # Errors
///
/// Returns I/O errors.
pub fn gc_with(
    repo: &Repository,
    now: SystemTime,
    resolved: Duration,
    unresolved: Duration,
) -> Result<GcReport, Error> {
    let cache = repo.common_dir().join("rr-cache");
    let entries = match fs::read_dir(&cache) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(GcReport::default()),
        Err(error) => return Err(error.into()),
    };

    let mut report = GcReport::default();
    for entry in entries {
        let entry = entry?;
        let Some(hash) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<ObjectId>().ok())
        else {
            continue;
        };
        let directory = entry.path();
        let older_than = |name: &str, age: Duration| -> Option<bool> {
            let modified = fs::metadata(directory.join(name)).ok()?.modified().ok()?;
            Some(now.duration_since(modified).unwrap_or_default() > age)
        };

        // Each variant expires on its own; the directory goes once none is left.
        let mut variants = Vec::new();
        for file in fs::read_dir(&directory)? {
            let name = file?.file_name();
            let Some(name) = name.to_str() else { continue };
            for prefix in ["preimage", "postimage"] {
                if let Some(suffix) = name.strip_prefix(prefix)
                    && !variants.contains(&suffix.to_owned())
                {
                    variants.push(suffix.to_owned());
                }
            }
        }
        for suffix in variants {
            let file = |name: &str| format!("{name}{suffix}");
            let expired = match older_than(&file("postimage"), resolved) {
                Some(expired) => expired,
                None => older_than(&file("preimage"), unresolved).unwrap_or(false),
            };
            if expired {
                for name in ["preimage", "postimage", "thisimage"] {
                    remove_if_present(&directory.join(file(name)))?;
                }
                report.removed.push(ConflictId {
                    hash,
                    variant: suffix
                        .strip_prefix('.')
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                });
            }
        }
        if fs::read_dir(&directory)?.next().is_none() {
            fs::remove_dir(&directory)?;
        }
    }
    Ok(report)
}

fn parse_merge_rr(bytes: &[u8]) -> Option<Vec<Tracked>> {
    let mut tracked = Vec::new();
    for record in bytes.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let tab = record.iter().position(|&b| b == b'\t')?;
        tracked.push(Tracked {
            id: ConflictId::parse(&record[..tab])?,
            path: record[tab + 1..].to_vec(),
        });
    }
    Some(tracked)
}

fn remove_if_present(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Writes through `<path>.lock` and a rename, as Git writes these files.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock = PathBuf::from(lock_name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| fs::rename(&lock, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&lock);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &[u8]) -> String {
        normalize(text, ObjectFormat::Sha1)
            .unwrap()
            .unwrap()
            .id
            .to_string()
    }

    #[test]
    fn ids_match_git() {
        // Observed with `git merge` and rerere.enabled; diff3 and zdiff3 give the same IDs.
        let two = b"a\n<<<<<<< HEAD\nAMAIN\n=======\nZSIDE\n>>>>>>> side\nc\nd\n<<<<<<< HEAD\nM2\n=======\nS2\n>>>>>>> side\nh\n";
        assert_eq!(id(two), "50316095e72d90e6d4970fdf7b430c30042ea7ee");
        let sorted = b"a\n<<<<<<< HEAD\nZMAIN\n=======\nASIDE\nASIDE2\n>>>>>>> side\nc\n";
        assert_eq!(id(sorted), "a3b84720f42e7d99640e0c4fd4eef3ada216e254");
        assert_eq!(
            normalize(sorted, ObjectFormat::Sha1)
                .unwrap()
                .unwrap()
                .preimage,
            b"a\n<<<<<<<\nASIDE\nASIDE2\n=======\nZMAIN\n>>>>>>>\nc\n"
        );
    }

    #[test]
    fn sha256_ids_use_the_repository_hash() {
        let text = b"a\n<<<<<<< HEAD\nMAIN\n=======\nSIDE\n>>>>>>> side\nc\n";
        let normalized = normalize(text, ObjectFormat::Sha256).unwrap().unwrap();
        assert_eq!(
            normalized.id.to_string(),
            "08c5d4dde43584b5c70c4778106af56e2354907c28f3b0fcf638e900f6e625da"
        );
    }

    #[test]
    fn malformed_markers_are_refused() {
        let unterminated = b"<<<<<<< a\nx\n=======\ny\n";
        assert!(matches!(
            normalize(unterminated, ObjectFormat::Sha1),
            Err(Error::Unterminated(1))
        ));
        let nested = b"<<<<<<< a\n<<<<<<< b\n";
        assert!(matches!(
            normalize(nested, ObjectFormat::Sha1),
            Err(Error::Nested(2))
        ));
        assert!(normalize(b"plain\n", ObjectFormat::Sha1).unwrap().is_none());
        // Longer runs are content, not markers.
        assert!(
            normalize(b"<<<<<<<<\n", ObjectFormat::Sha1)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn merge_rr_round_trips() {
        let bytes = b"27d08dca9e6e45868a32cd28625ad61784a53d0e\tf\x0027d08dca9e6e45868a32cd28625ad61784a53d0e.1\tg h\x00";
        let tracked = parse_merge_rr(bytes).unwrap();
        assert_eq!(tracked[1].id.variant, 1);
        assert_eq!(tracked[1].path, b"g h");
        assert_eq!(
            tracked[1].id.to_string(),
            "27d08dca9e6e45868a32cd28625ad61784a53d0e.1"
        );
    }
}
