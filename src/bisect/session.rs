//! A bisection's persistent state, stored as Git stores it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::{fs, io};

use super::{Error, Scope, Selection};
use crate::pathspec::{Context, Pathspec};
use crate::refs::{Dereference, Expected, RefEdit, RefName, Reflog, Target};
use crate::{ObjectId, Repository, Signature};

/// State files Git keeps in the repository's Git directory during a bisection.
const FILES: [&str; 9] = [
    "BISECT_HEAD",
    "BISECT_START",
    "BISECT_TERMS",
    "BISECT_NAMES",
    "BISECT_LOG",
    "BISECT_EXPECTED_REV",
    "BISECT_ANCESTORS_OK",
    "BISECT_FIRST_PARENT",
    "BISECT_RUN",
];

/// The words a bisection uses for the two states, `bad`/`good` by default.
///
/// The new term marks commits that have the property being searched for, such as a bug; the old
/// term marks commits from before it appeared. Git stores them in `BISECT_TERMS` and uses them in
/// reference names (`refs/bisect/<new>`, `refs/bisect/<old>-<oid>`) and messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terms {
    new: String,
    old: String,
}

impl Default for Terms {
    fn default() -> Self {
        Self {
            new: "bad".into(),
            old: "good".into(),
        }
    }
}

impl Terms {
    /// Custom terms, as `--term-new` and `--term-old` set them.
    ///
    /// ```
    /// use girt::bisect::Terms;
    /// let terms = Terms::new("broken", "fixed")?;
    /// assert_eq!((terms.new_term(), terms.old_term()), ("broken", "fixed"));
    /// assert!(Terms::new("same", "same").is_err());
    /// assert!(Terms::new("skip", "fixed").is_err());
    /// # Ok::<(), girt::bisect::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Term`] when the terms are equal, are not valid in a reference name, or
    /// name a bisect subcommand other than the `bad`/`good` and `new`/`old` pairs.
    pub fn new(new: impl Into<String>, old: impl Into<String>) -> Result<Self, Error> {
        let terms = Self {
            new: new.into(),
            old: old.into(),
        };
        for term in [&terms.new, &terms.old] {
            check_term(term)?;
        }
        if terms.new == terms.old {
            return Err(Error::Term(terms.new));
        }
        Ok(terms)
    }

    /// The term for commits that have the property, `bad` by default.
    pub fn new_term(&self) -> &str {
        &self.new
    }

    /// The term for commits from before the property appeared, `good` by default.
    pub fn old_term(&self) -> &str {
        &self.old
    }
}

fn check_term(term: &str) -> Result<(), Error> {
    const RESERVED: [&str; 11] = [
        "help",
        "start",
        "skip",
        "next",
        "reset",
        "visualize",
        "view",
        "replay",
        "log",
        "run",
        "terms",
    ];
    let valid = !term.is_empty()
        && !RESERVED.contains(&term)
        && RefName::new(format!("refs/bisect/{term}")).is_ok();
    if valid {
        Ok(())
    } else {
        Err(Error::Term(term.to_owned()))
    }
}

/// A verdict on a commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Mark {
    /// The commit has the property (`bad` by default).
    New,
    /// The commit is from before the property appeared (`good` by default).
    Old,
    /// The commit cannot be tested.
    Skip,
}

/// Where a bisection goes after a verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Step {
    /// Waiting for a new commit, old commits, or both.
    Waiting {
        /// Whether a new commit is known.
        new: bool,
        /// How many old commits are known.
        olds: usize,
    },
    /// An old commit is not an ancestor of the new one, so this merge base must be tested first.
    MergeBase(ObjectId),
    /// Test this commit next; it has been checked out, or written to `BISECT_HEAD`.
    Test {
        /// The commit to test.
        commit: ObjectId,
        /// Git's "N revisions left to test after this" count.
        remaining: usize,
        /// Git's "roughly N steps" estimate.
        steps: usize,
    },
    /// The first new commit.
    Found(ObjectId),
    /// Only skipped commits remain; any of these could be the first new commit.
    OnlySkipped(Vec<ObjectId>),
}

/// Options for [`Bisect::start`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Start {
    /// A commit with the property, if already known.
    pub new: Option<ObjectId>,
    /// Commits from before the property appeared.
    pub olds: Vec<ObjectId>,
    /// The words for the two states.
    pub terms: Terms,
    /// Restrict the search to commits changing these paths, stored as given.
    pub paths: Vec<Vec<u8>>,
    /// Follow only first parents.
    pub first_parent: bool,
    /// Record the commit to test in `BISECT_HEAD` instead of checking it out.
    pub no_checkout: bool,
}

impl Start {
    /// Sets [`Self::new`].
    #[must_use]
    pub fn with_new(mut self, new: ObjectId) -> Self {
        self.new = Some(new);
        self
    }

    /// Sets [`Self::olds`].
    #[must_use]
    pub fn with_olds(mut self, olds: Vec<ObjectId>) -> Self {
        self.olds = olds;
        self
    }

    /// Sets [`Self::terms`].
    #[must_use]
    pub fn with_terms(mut self, terms: Terms) -> Self {
        self.terms = terms;
        self
    }

    /// Sets [`Self::paths`].
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<Vec<u8>>) -> Self {
        self.paths = paths;
        self
    }

    /// Sets [`Self::first_parent`].
    #[must_use]
    pub fn with_first_parent(mut self, first_parent: bool) -> Self {
        self.first_parent = first_parent;
        self
    }

    /// Sets [`Self::no_checkout`].
    #[must_use]
    pub fn with_no_checkout(mut self, no_checkout: bool) -> Self {
        self.no_checkout = no_checkout;
        self
    }
}

/// What [`Bisect::reset`] returns to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResetTo {
    /// The branch or commit checked out when the bisection started.
    #[default]
    Original,
    /// This commit, detached.
    Commit(ObjectId),
    /// Leave HEAD and the working tree where they are.
    Current,
}

/// A caller's verdict for [`Bisect::run`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verdict {
    /// The commit has the property.
    New,
    /// The commit does not have the property.
    Old,
    /// The commit cannot be tested.
    Skip,
    /// Stop the run, leaving the bisection where it is.
    Abort,
}

impl Verdict {
    /// Interprets a `git bisect run` command's exit code.
    ///
    /// 0 is old, 125 is skip, 1 to 127 otherwise are new, and 128 or more (or a signal) aborts.
    ///
    /// ```
    /// use girt::bisect::Verdict;
    /// assert_eq!(Verdict::from_exit_code(Some(0)), Verdict::Old);
    /// assert_eq!(Verdict::from_exit_code(Some(1)), Verdict::New);
    /// assert_eq!(Verdict::from_exit_code(Some(125)), Verdict::Skip);
    /// assert_eq!(Verdict::from_exit_code(Some(128)), Verdict::Abort);
    /// assert_eq!(Verdict::from_exit_code(None), Verdict::Abort);
    /// ```
    pub fn from_exit_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Old,
            Some(125) => Self::Skip,
            Some(1..=127) => Self::New,
            _ => Self::Abort,
        }
    }
}

/// An in-progress bisection, read from or written to Git's state files.
///
/// Git and girt use the same files and references, so either can continue a bisection the other
/// started. Mutating calls append to `BISECT_LOG` as Git does, so `git bisect log` and
/// [`Bisect::replay`] can reproduce the session. Reflog entries for HEAD use the committer given
/// to [`Self::open`] or [`Self::start`].
///
/// Callers must hold exclusive access to the repository's HEAD, index and working tree.
#[derive(Debug)]
pub struct Bisect<'repo> {
    repo: &'repo Repository,
    committer: Signature,
    terms: Terms,
    start: Vec<u8>,
    paths: Vec<Vec<u8>>,
    first_parent: bool,
    no_checkout: bool,
    new: Option<ObjectId>,
    olds: Vec<ObjectId>,
    skipped: Vec<ObjectId>,
}

impl<'repo> Bisect<'repo> {
    /// Reads the bisection in progress, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when state files cannot be read, [`Error::State`] when they are
    /// malformed, and [`Error::Reference`] when `refs/bisect/*` cannot be listed.
    pub fn open(repo: &'repo Repository, committer: Signature) -> Result<Option<Self>, Error> {
        let git_dir = repo.git_dir();
        let start = match fs::read(git_dir.join("BISECT_START")) {
            Ok(start) => start,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let terms = match fs::read_to_string(git_dir.join("BISECT_TERMS")) {
            Ok(text) => {
                let mut lines = text.lines();
                match (lines.next(), lines.next()) {
                    (Some(new), Some(old)) => Terms::new(new, old)?,
                    _ => return Err(Error::State("BISECT_TERMS")),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Terms::default(),
            Err(error) => return Err(error.into()),
        };
        let paths = match fs::read(git_dir.join("BISECT_NAMES")) {
            Ok(bytes) => parse_names(&bytes).ok_or(Error::State("BISECT_NAMES"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };

        let mut bisect = Self {
            repo,
            committer,
            terms,
            start: trim_newline(&start).to_vec(),
            paths,
            first_parent: git_dir.join("BISECT_FIRST_PARENT").exists(),
            no_checkout: false,
            new: None,
            olds: Vec::new(),
            skipped: Vec::new(),
        };
        bisect.read_refs()?;
        Ok(Some(bisect))
    }

    /// Starts a bisection, as `git bisect start` does.
    ///
    /// Records where HEAD is, writes the terms, pathspec and options, marks the given commits and
    /// moves to the first commit to test when both a new and an old commit are known.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InProgress`] when a bisection already exists, [`Error::NoHead`] when HEAD
    /// is unborn, and the errors of [`Self::next_step`].
    pub fn start(
        repo: &'repo Repository,
        committer: Signature,
        options: Start,
    ) -> Result<(Self, Step), Error> {
        if repo.git_dir().join("BISECT_START").exists() {
            return Err(Error::InProgress);
        }
        let references = repo.references()?;
        let start = match references.read(&RefName::HEAD)? {
            Some(Target::Symbolic(name)) => match name.as_bytes().strip_prefix(b"refs/heads/") {
                Some(branch) => branch.to_vec(),
                None => name.as_bytes().to_vec(),
            },
            Some(Target::Direct(id)) => id.to_string().into_bytes(),
            None => return Err(Error::NoHead),
        };
        if repo.head()?.id.is_none() {
            return Err(Error::NoHead);
        }
        if !options.no_checkout && repo.worktree().is_none() {
            return Err(Error::Bare);
        }
        if options.no_checkout && repo.reference_backend() == crate::refs::Backend::Reftable {
            // Git keeps BISECT_HEAD in the table there, which girt does not write yet.
            return Err(Error::Unsupported("--no-checkout in reftable repositories"));
        }

        let mut bisect = Self {
            repo,
            committer,
            terms: options.terms.clone(),
            start: start.clone(),
            paths: options.paths.clone(),
            first_parent: options.first_parent,
            no_checkout: false,
            new: None,
            olds: Vec::new(),
            skipped: Vec::new(),
        };
        // Validate the pathspec before writing anything.
        bisect.scope()?;

        let result = bisect.write_start(&options);
        if let Err(error) = result {
            let _ = bisect.remove_state();
            return Err(error);
        }
        let step = bisect.next_step()?;
        Ok((bisect, step))
    }

    fn write_start(&mut self, options: &Start) -> Result<(), Error> {
        let git_dir = self.repo.git_dir();
        let mut log = Vec::new();
        if let Some(new) = options.new {
            log.extend(self.log_comment(self.terms.new_term(), new)?);
        }
        for &old in &options.olds {
            log.extend(self.log_comment(self.terms.old_term(), old)?);
        }

        let mut words: Vec<Vec<u8>> = Vec::new();
        if self.terms != Terms::default() {
            words.push(format!("--term-new={}", self.terms.new_term()).into_bytes());
            words.push(format!("--term-old={}", self.terms.old_term()).into_bytes());
        }
        if options.no_checkout {
            words.push(b"--no-checkout".to_vec());
        }
        if options.first_parent {
            words.push(b"--first-parent".to_vec());
        }
        words.extend(options.new.iter().map(|id| id.to_string().into_bytes()));
        words.extend(options.olds.iter().map(|id| id.to_string().into_bytes()));
        let names = names_line(&self.paths);
        if !self.paths.is_empty() {
            words.push(b"--".to_vec());
            words.extend(self.paths.iter().cloned());
        }
        log.extend_from_slice(b"git bisect start");
        for word in &words {
            log.push(b' ');
            log.extend(quote(word));
        }
        log.push(b'\n');

        fs::write(
            git_dir.join("BISECT_START"),
            [&self.start[..], b"\n"].concat(),
        )?;
        fs::write(
            git_dir.join("BISECT_TERMS"),
            format!("{}\n{}\n", self.terms.new_term(), self.terms.old_term()),
        )?;
        fs::write(git_dir.join("BISECT_NAMES"), names)?;
        if options.first_parent {
            fs::write(git_dir.join("BISECT_FIRST_PARENT"), b"")?;
        }
        if options.no_checkout {
            let head = self.repo.head()?.id.ok_or(Error::NoHead)?;
            self.set_bisect_head(head)?;
        }

        if let Some(new) = options.new {
            self.write_mark(Mark::New, new)?;
        }
        for &old in &options.olds {
            self.write_mark(Mark::Old, old)?;
        }
        fs::write(git_dir.join("BISECT_LOG"), log)?;
        Ok(())
    }

    /// The words for the two states.
    pub fn terms(&self) -> &Terms {
        &self.terms
    }

    /// The commit marked new, if any.
    pub fn new_commit(&self) -> Option<ObjectId> {
        self.new
    }

    /// The commits marked old, in reference-name order.
    pub fn old_commits(&self) -> &[ObjectId] {
        &self.olds
    }

    /// The commits marked skip, in reference-name order.
    pub fn skipped(&self) -> &[ObjectId] {
        &self.skipped
    }

    /// Whether the bisection records its position in `BISECT_HEAD` instead of checking out.
    pub fn is_no_checkout(&self) -> bool {
        self.no_checkout
    }

    /// The pathspec arguments recorded in `BISECT_NAMES`.
    pub fn paths(&self) -> &[Vec<u8>] {
        &self.paths
    }

    /// The history this bisection searches.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Pathspec`] when the recorded pathspec does not parse.
    pub fn scope(&self) -> Result<Scope, Error> {
        let paths = if self.paths.is_empty() {
            None
        } else {
            Some(Pathspec::parse(&self.paths, &Context::new())?)
        };
        Ok(Scope::default()
            .with_first_parent(self.first_parent)
            .with_paths(paths))
    }

    /// The commit being tested: `BISECT_HEAD` with `--no-checkout`, otherwise HEAD.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reference`] when the reference cannot be read and [`Error::NoHead`] when
    /// it does not name a commit.
    pub fn current(&self) -> Result<ObjectId, Error> {
        if self.no_checkout {
            return read_bisect_head(self.repo)?.ok_or(Error::State("BISECT_HEAD"));
        }
        self.repo
            .references()?
            .resolve(&RefName::HEAD)?
            .id
            .ok_or(Error::NoHead)
    }

    /// The commits the next step chooses from, as `git bisect visualize` lists them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Waiting`] until both a new and an old commit are known, and the errors of
    /// [`crate::Objects::bisect_candidates`].
    pub fn candidates(&self) -> Result<super::Candidates, Error> {
        let new = self.new.ok_or(Error::Waiting)?;
        if self.olds.is_empty() {
            return Err(Error::Waiting);
        }
        self.repo
            .objects()?
            .bisect_candidates(new, &self.olds, &self.scope()?)
    }

    /// Records verdicts on commits and moves to the next commit to test.
    ///
    /// Marking a second commit new replaces the first. Each verdict is appended to `BISECT_LOG`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::next_step`] and storage errors for the references and log.
    pub fn mark(&mut self, mark: Mark, commits: &[ObjectId]) -> Result<Step, Error> {
        self.record(mark, commits)?;
        self.next_step()
    }

    fn record(&mut self, mark: Mark, commits: &[ObjectId]) -> Result<(), Error> {
        // A new verdict can introduce an old commit that is not an ancestor of the new one, so
        // ancestry is checked again, as Git does.
        if mark != Mark::Skip {
            let marker = self.repo.git_dir().join("BISECT_ANCESTORS_OK");
            match fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut log = Vec::new();
        for &commit in commits {
            let term = self.term(mark);
            log.extend(self.log_comment(term, commit)?);
            log.extend(format!("git bisect {term} {commit}\n").into_bytes());
            self.write_mark(mark, commit)?;
        }
        self.append_log(&log)
    }

    fn term(&self, mark: Mark) -> &str {
        match mark {
            Mark::New => self.terms.new_term(),
            Mark::Old => self.terms.old_term(),
            Mark::Skip => "skip",
        }
    }

    /// Chooses the next commit and checks it out, as Git does after every verdict.
    ///
    /// Before the first search, old commits that are not ancestors of the new commit send the
    /// bisection to their merge bases. When the first new commit is found, `BISECT_LOG` records
    /// it and the state stays in place until [`Self::reset`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAncestors`] when the new commit is itself a merge base of an old
    /// commit, [`Error::Untestable`] when no candidate changes the bisected paths,
    /// [`Error::Checkout`] when the working tree cannot be updated, and history, reference and
    /// storage errors.
    pub fn next_step(&mut self) -> Result<Step, Error> {
        let Some(new) = self.new else {
            return Ok(Step::Waiting {
                new: false,
                olds: self.olds.len(),
            });
        };
        if self.olds.is_empty() {
            return Ok(Step::Waiting { new: true, olds: 0 });
        }

        if let Some(base) = self.untested_merge_base(new)? {
            self.check_out(base)?;
            return Ok(Step::MergeBase(base));
        }

        let candidates = self.candidates()?;
        let skipped: HashSet<ObjectId> = self.skipped.iter().copied().collect();
        match candidates.select(&skipped) {
            Selection::Test {
                commit,
                remaining,
                steps,
            } => {
                self.check_out(commit)?;
                Ok(Step::Test {
                    commit,
                    remaining,
                    steps,
                })
            }
            Selection::Found(commit) => {
                let comment =
                    self.log_comment(&format!("first '{}' commit", self.terms.new_term()), commit)?;
                self.append_log(&comment)?;
                Ok(Step::Found(commit))
            }
            Selection::OnlySkipped(possible) => {
                let mut log = b"# only skipped commits left to test\n".to_vec();
                let label = format!("possible first '{}' commit", self.terms.new_term());
                for &commit in possible.iter().rev() {
                    log.extend(self.log_comment(&label, commit)?);
                }
                self.append_log(&log)?;
                Ok(Step::OnlySkipped(possible))
            }
            Selection::Untestable => Err(Error::Untestable),
        }
    }

    /// The first merge base that still needs testing, after checking the others.
    fn untested_merge_base(&mut self, new: ObjectId) -> Result<Option<ObjectId>, Error> {
        let marker = self.repo.git_dir().join("BISECT_ANCESTORS_OK");
        if marker.exists() {
            return Ok(None);
        }
        let objects = self.repo.objects()?;
        let mut unrelated = Vec::new();
        for &old in &self.olds {
            if !objects.is_ancestor(old, new)? {
                unrelated.push(old);
            }
        }

        for &old in &unrelated {
            for base in objects.merge_bases(new, old)? {
                if base == new {
                    return Err(Error::NotAncestors { base });
                }
                if self.olds.contains(&base) || self.skipped.contains(&base) {
                    continue;
                }
                return Ok(Some(base));
            }
        }
        fs::write(marker, b"")?;
        Ok(None)
    }

    /// Ends the bisection: returns HEAD to `to` and removes the state files and references.
    ///
    /// With `--no-checkout` the working tree is never touched.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Checkout`] when the working tree cannot be updated, and reference and
    /// storage errors. State is removed only after the checkout succeeds.
    pub fn reset(mut self, to: ResetTo) -> Result<(), Error> {
        if !self.no_checkout {
            match to {
                ResetTo::Current => {}
                ResetTo::Commit(commit) => self.check_out(commit)?,
                ResetTo::Original => self.check_out_original()?,
            }
        }
        self.remove_state()
    }

    /// The contents of `BISECT_LOG`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the log cannot be read.
    pub fn log(&self) -> Result<Vec<u8>, Error> {
        Ok(fs::read(self.repo.git_dir().join("BISECT_LOG"))?)
    }

    /// Replays a `BISECT_LOG`, as `git bisect replay` does.
    ///
    /// Any bisection in progress is reset first. Comment lines are ignored; `git bisect start`,
    /// verdict and `terms` lines are applied in order, without checking out the intermediate
    /// commits. Revision arguments must be full object IDs, which is how girt and Git write them
    /// for verdicts; the start line's revisions are resolved with the repository's revision
    /// syntax.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Replay`] for a line that is not a bisect command, and the errors of
    /// [`Self::start`] and [`Self::mark`].
    pub fn replay(
        repo: &'repo Repository,
        committer: Signature,
        log: &[u8],
    ) -> Result<(Self, Step), Error> {
        if let Some(existing) = Self::open(repo, committer.clone())? {
            existing.reset(ResetTo::Original)?;
        }

        let mut bisect: Option<Self> = None;
        for (number, line) in log.split(|&b| b == b'\n').enumerate() {
            let line = trim_newline(line);
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            let words = unquote_words(line).ok_or(Error::Replay(number + 1))?;
            let words: Vec<&[u8]> = words.iter().map(Vec::as_slice).collect();
            let [b"git", b"bisect", command, arguments @ ..] = words.as_slice() else {
                return Err(Error::Replay(number + 1));
            };

            if *command == b"start" {
                let options = parse_start(repo, arguments).ok_or(Error::Replay(number + 1))?;
                let start = Self::start_quiet(repo, committer.clone(), options)?;
                bisect = Some(start);
                continue;
            }
            let session = bisect.as_mut().ok_or(Error::Replay(number + 1))?;
            let mark = if *command == session.terms.new_term().as_bytes() {
                Mark::New
            } else if *command == session.terms.old_term().as_bytes() {
                Mark::Old
            } else if *command == b"skip" {
                Mark::Skip
            } else {
                return Err(Error::Replay(number + 1));
            };
            let commits = arguments
                .iter()
                .map(|word| {
                    std::str::from_utf8(word)
                        .ok()
                        .and_then(|text| text.parse().ok())
                        .ok_or(Error::Replay(number + 1))
                })
                .collect::<Result<Vec<ObjectId>, Error>>()?;
            session.record(mark, &commits)?;
        }

        let mut bisect = bisect.ok_or(Error::Replay(0))?;
        let step = bisect.next_step()?;
        Ok((bisect, step))
    }

    fn start_quiet(
        repo: &'repo Repository,
        committer: Signature,
        options: Start,
    ) -> Result<Self, Error> {
        let mut bisect = Self {
            repo,
            committer,
            terms: options.terms.clone(),
            start: Vec::new(),
            paths: options.paths.clone(),
            first_parent: options.first_parent,
            no_checkout: false,
            new: None,
            olds: Vec::new(),
            skipped: Vec::new(),
        };
        bisect.start = match repo.references()?.read(&RefName::HEAD)? {
            Some(Target::Symbolic(name)) => name
                .as_bytes()
                .strip_prefix(b"refs/heads/")
                .unwrap_or(name.as_bytes())
                .to_vec(),
            Some(Target::Direct(id)) => id.to_string().into_bytes(),
            None => return Err(Error::NoHead),
        };
        bisect.scope()?;
        bisect.write_start(&options)?;
        Ok(bisect)
    }

    /// Runs `test` on each commit to test until the first new commit is found, as
    /// `git bisect run` does.
    ///
    /// The bisection must already have a commit checked out to test, so start it with both a new
    /// and an old commit. Each verdict is recorded as [`Self::mark`] records it. A
    /// [`Verdict::Abort`] stops with the bisection left at the commit being tested.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Aborted`] when `test` aborts, [`Error::Waiting`] when no commit is ready
    /// to test, and the errors of [`Self::mark`].
    pub fn run(&mut self, mut test: impl FnMut(ObjectId) -> Verdict) -> Result<Step, Error> {
        // Git tests the commit already checked out when it is the one bisect chose.
        let expected = fs::read_to_string(self.repo.git_dir().join("BISECT_EXPECTED_REV"))
            .ok()
            .and_then(|text| text.trim().parse::<ObjectId>().ok());
        let mut commit = match expected {
            Some(expected) if self.current()? == expected => expected,
            _ => match self.next_step()? {
                Step::Test { commit, .. } | Step::MergeBase(commit) => commit,
                Step::Waiting { .. } => return Err(Error::Waiting),
                step => return Ok(step),
            },
        };

        loop {
            let mark = match test(commit) {
                Verdict::New => Mark::New,
                Verdict::Old => Mark::Old,
                Verdict::Skip => Mark::Skip,
                Verdict::Abort => return Err(Error::Aborted(commit)),
            };
            commit = match self.mark(mark, &[commit])? {
                Step::Test { commit, .. } | Step::MergeBase(commit) => commit,
                Step::Waiting { .. } => return Err(Error::Waiting),
                step => return Ok(step),
            };
        }
    }

    fn read_refs(&mut self) -> Result<(), Error> {
        let references = self.repo.references()?;
        let new_name = format!("refs/bisect/{}", self.terms.new_term());
        let old_prefix = format!("refs/bisect/{}-", self.terms.old_term());
        for reference in references.list_prefix(b"refs/bisect/")? {
            let name = reference.name.as_bytes();
            let Target::Direct(id) = reference.target else {
                continue;
            };
            if name == new_name.as_bytes() {
                self.new = Some(id);
            } else if name.starts_with(old_prefix.as_bytes()) {
                self.olds.push(id);
            } else if name.starts_with(b"refs/bisect/skip-") {
                self.skipped.push(id);
            }
        }
        self.no_checkout = read_bisect_head(self.repo)?.is_some();
        Ok(())
    }

    fn write_mark(&mut self, mark: Mark, commit: ObjectId) -> Result<(), Error> {
        let name = match mark {
            Mark::New => format!("refs/bisect/{}", self.terms.new_term()),
            Mark::Old => format!("refs/bisect/{}-{commit}", self.terms.old_term()),
            Mark::Skip => format!("refs/bisect/skip-{commit}"),
        };
        let name = RefName::new(name).map_err(|_| Error::Term(self.term(mark).to_owned()))?;
        let edit = RefEdit::new(
            name,
            Some(Target::Direct(commit)),
            Expected::Any,
            Reflog::Preserve,
        );
        self.repo.references()?.transaction(&[edit])?;

        match mark {
            Mark::New => self.new = Some(commit),
            Mark::Old if !self.olds.contains(&commit) => self.olds.push(commit),
            Mark::Skip if !self.skipped.contains(&commit) => self.skipped.push(commit),
            Mark::Old | Mark::Skip => {}
        }
        Ok(())
    }

    /// Writes `BISECT_HEAD` through a lock file, as Git writes pseudo-references in a files
    /// repository.
    fn set_bisect_head(&mut self, commit: ObjectId) -> Result<(), Error> {
        use std::io::Write;

        let git_dir = self.repo.git_dir();
        let lock = git_dir.join("BISECT_HEAD.lock");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)?;
        let written = writeln!(file, "{commit}").and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = written.and_then(|()| fs::rename(&lock, git_dir.join("BISECT_HEAD"))) {
            let _ = fs::remove_file(&lock);
            return Err(error.into());
        }
        self.no_checkout = true;
        Ok(())
    }

    /// Moves to `commit`: updates `BISECT_HEAD`, or checks it out with a detached HEAD.
    fn check_out(&mut self, commit: ObjectId) -> Result<(), Error> {
        fs::write(
            self.repo.git_dir().join("BISECT_EXPECTED_REV"),
            format!("{commit}\n"),
        )?;
        if self.no_checkout {
            return self.set_bisect_head(commit);
        }
        let label = commit.to_string();
        self.switch(Target::Direct(commit), commit, label.as_bytes())
    }

    fn check_out_original(&self) -> Result<(), Error> {
        let start = &self.start;
        if let Ok(id) = std::str::from_utf8(start)
            .unwrap_or_default()
            .parse::<ObjectId>()
        {
            return self.switch(Target::Direct(id), id, start);
        }
        let branch = RefName::branch(start).map_err(|_| Error::State("BISECT_START"))?;
        let id = self
            .repo
            .references()?
            .resolve(&branch)?
            .id
            .ok_or(Error::State("BISECT_START"))?;
        self.switch(Target::Symbolic(branch), id, start)
    }

    /// Checks out `commit`'s tree over HEAD's, then points HEAD at `target`.
    fn switch(&self, target: Target, commit: ObjectId, label: &[u8]) -> Result<(), Error> {
        let references = self.repo.references()?;
        let stored = references.read(&RefName::HEAD)?;
        let from = match &stored {
            Some(Target::Symbolic(name)) => name
                .as_bytes()
                .strip_prefix(b"refs/heads/")
                .unwrap_or(name.as_bytes())
                .to_vec(),
            Some(Target::Direct(id)) => id.to_string().into_bytes(),
            None => Vec::new(),
        };
        let objects = self.repo.objects()?;
        let baseline = match references.resolve(&RefName::HEAD)?.id {
            Some(id) => Some(objects.read_commit(id)?.tree()),
            None => None,
        };
        if stored.as_ref() == Some(&target) {
            return Ok(());
        }
        let tree = objects.read_commit(commit)?.tree();

        // Lock HEAD before touching files, so a held HEAD lock fails before any checkout.
        let message = [b"checkout: moving from ".as_slice(), &from, b" to ", label].concat();
        let expected = stored.map_or(Expected::Absent, Expected::Value);
        let edit = RefEdit::new(
            RefName::HEAD,
            Some(target),
            expected,
            Reflog::Append {
                committer: self.committer.clone(),
                message,
            },
        )
        .with_dereference(Dereference::Stored);
        let prepared = references.prepare_transaction(&[edit])?;

        if baseline != Some(tree) {
            self.repo
                .checkout_tree_configured(baseline, Some(tree))
                .map_err(|failure| Error::Checkout(Box::new(failure)))?;
        }
        prepared.publish()?;
        Ok(())
    }

    fn remove_state(&self) -> Result<(), Error> {
        let references = self.repo.references()?;
        let mut edits = Vec::new();
        for reference in references.list_prefix(b"refs/bisect/")? {
            if let Target::Direct(id) = reference.target {
                edits.push(RefEdit::delete(reference.name, id, Reflog::Delete));
            }
        }
        if !edits.is_empty() {
            references.transaction(&edits)?;
        }

        for file in FILES {
            match fs::remove_file(self.repo.git_dir().join(file)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn append_log(&self, bytes: &[u8]) -> Result<(), Error> {
        use std::io::Write;

        let path: PathBuf = self.repo.git_dir().join("BISECT_LOG");
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(bytes)?;
        Ok(())
    }

    /// `# <label>: [<oid>] <subject>`, the comment Git writes before each verdict.
    fn log_comment(&self, label: &str, commit: ObjectId) -> Result<Vec<u8>, Error> {
        let message = self.repo.objects()?.read_commit(commit)?;
        let subject = subject(message.message());
        Ok([
            format!("# {label}: [{commit}] ").as_bytes(),
            &subject,
            b"\n",
        ]
        .concat())
    }
}

/// Reads the `BISECT_HEAD` pseudo-reference file, if present.
fn read_bisect_head(repo: &Repository) -> Result<Option<ObjectId>, Error> {
    match fs::read_to_string(repo.git_dir().join("BISECT_HEAD")) {
        Ok(text) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| Error::State("BISECT_HEAD")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The subject Git prints for `%s`: the first paragraph with line breaks joined by spaces.
fn subject(message: &[u8]) -> Vec<u8> {
    let mut subject = Vec::new();
    for line in message.split(|&b| b == b'\n') {
        let line = trim_newline(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            if subject.is_empty() {
                continue;
            }
            break;
        }
        if !subject.is_empty() {
            subject.push(b' ');
        }
        subject.extend_from_slice(line.trim_ascii());
    }
    subject
}

fn trim_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

/// Single-quotes a word for a shell, as Git writes bisect arguments.
pub(super) fn quote(word: &[u8]) -> Vec<u8> {
    let mut quoted = vec![b'\''];
    for &byte in word {
        if byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

/// `BISECT_NAMES`: a space before each quoted word, `--` first, or a lone newline.
fn names_line(paths: &[Vec<u8>]) -> Vec<u8> {
    let mut line = Vec::new();
    if !paths.is_empty() {
        line.extend_from_slice(b" '--'");
        for path in paths {
            line.push(b' ');
            line.extend(quote(path));
        }
    }
    line.push(b'\n');
    line
}

fn parse_names(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut words = unquote_words(trim_newline(bytes))?;
    if words.first().is_some_and(|word| word == b"--") {
        words.remove(0);
    }
    Some(words)
}

/// Splits a line of single-quoted or bare words, undoing [`quote`].
fn unquote_words(line: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut words = Vec::new();
    let mut rest = line;
    loop {
        rest = rest.trim_ascii_start();
        if rest.is_empty() {
            return Some(words);
        }
        let mut word = Vec::new();
        while let Some((&byte, tail)) = rest.split_first() {
            match byte {
                b' ' | b'\t' => break,
                b'\'' => {
                    let end = tail.iter().position(|&b| b == b'\'')?;
                    word.extend_from_slice(&tail[..end]);
                    rest = &tail[end + 1..];
                    continue;
                }
                b'\\' => {
                    let (&escaped, after) = tail.split_first()?;
                    word.push(escaped);
                    rest = after;
                    continue;
                }
                _ => word.push(byte),
            }
            rest = tail;
        }
        words.push(word);
    }
}

/// Reads `git bisect start` arguments back into options.
fn parse_start(repo: &Repository, arguments: &[&[u8]]) -> Option<Start> {
    let mut options = Start::default();
    let mut new_term = None;
    let mut old_term = None;
    let mut revisions = Vec::new();
    let mut iter = arguments.iter();
    while let Some(&word) = iter.next() {
        let text = std::str::from_utf8(word).ok()?;
        if word == b"--" {
            options.paths = iter.map(|w| w.to_vec()).collect();
            break;
        } else if let Some(term) = text
            .strip_prefix("--term-new=")
            .or(text.strip_prefix("--term-bad="))
        {
            new_term = Some(term.to_owned());
        } else if let Some(term) = text
            .strip_prefix("--term-old=")
            .or(text.strip_prefix("--term-good="))
        {
            old_term = Some(term.to_owned());
        } else if text == "--no-checkout" {
            options.no_checkout = true;
        } else if text == "--first-parent" {
            options.first_parent = true;
        } else {
            revisions.push(text.to_owned());
        }
    }
    if new_term.is_some() || old_term.is_some() {
        options.terms = Terms::new(
            new_term.unwrap_or_else(|| "bad".into()),
            old_term.unwrap_or_else(|| "good".into()),
        )
        .ok()?;
    }

    let objects = repo.objects().ok()?;
    let resolver = crate::revision::Resolver::new(repo, &objects).ok()?;
    let mut ids = revisions.iter().map(|revision| {
        let revision = revision.parse().ok()?;
        resolver
            .resolve_to(&revision, crate::ObjectKind::Commit)
            .ok()
    });
    if let Some(first) = ids.next() {
        options.new = Some(first?);
        options.olds = ids.collect::<Option<Vec<_>>>()?;
    }
    Some(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_git_bytes() {
        let paths = vec![b"d".to_vec(), b"f 3".to_vec(), b"it's".to_vec()];
        let line = names_line(&paths);
        assert_eq!(line, b" '--' 'd' 'f 3' 'it'\\''s'\n");
        assert_eq!(parse_names(&line), Some(paths));
        assert_eq!(names_line(&[]), b"\n");
        assert_eq!(parse_names(b"\n"), Some(vec![]));
    }

    #[test]
    fn subject_joins_first_paragraph() {
        assert_eq!(subject(b"c5\n\nbody 5\n"), b"c5");
        assert_eq!(subject(b"one\ntwo\n\nbody"), b"one two");
    }

    #[test]
    fn reserved_terms_are_refused() {
        assert!(Terms::new("new", "old").is_ok());
        assert!(Terms::new("run", "old").is_err());
        assert!(Terms::new("a b", "old").is_err());
    }
}
