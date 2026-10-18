//! `girt log` and `girt history`: commit history walks and ancestry queries.

use std::collections::{BinaryHeap, HashSet};
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _, eyre};
use girt::display::DateFormat;
use girt::revision::Selection;
use girt::{
    Commit, HistoryLimits, IdentityDate, IdentityRef, ObjectId, Objects, ReadLimits,
    TreeCompareLimits,
};

use crate::common::{self, display_bytes};
use crate::{Context, complete};

/// History queries.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Count commits reachable from revisions (like `git rev-list --count`).
    Count {
        /// Revisions and ranges: `A`, `^A`, `A..B` or `A...B`.
        #[arg(required = true, add = ArgValueCompleter::new(complete::revisions))]
        revs: Vec<String>,
    },
    /// Print the best common ancestor of two commits (like `git merge-base`).
    ///
    /// Exits with status 1 when the commits share no history.
    MergeBase {
        /// First commit.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        a: String,
        /// Second commit.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        b: String,
        /// Print every merge base, not just the first.
        #[arg(long)]
        all: bool,
    },
    /// Name a commit by the nearest tag (like `git describe`).
    ///
    /// Prints `<tag>-<count>-g<id>`: the commit is `<count>` commits after `<tag>`. A commit that
    /// is itself tagged prints just the tag. Counts are exact; on merge-heavy history `git
    /// describe` estimates and can print a larger count or a different tag.
    Describe {
        /// Commit to describe; `HEAD` when omitted.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        rev: Option<String>,
        /// Append `-dirty`, or MARK, when tracked files or the index differ from `HEAD`.
        /// Untracked files do not count. Describes `HEAD` only.
        #[arg(
            long,
            value_name = "MARK",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "-dirty"
        )]
        dirty: Option<String>,
        /// Like `--dirty`, and append `-broken`, or MARK, when the working tree cannot be
        /// compared, such as with a damaged index.
        #[arg(
            long,
            value_name = "MARK",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "-broken"
        )]
        broken: Option<String>,
        /// Also use lightweight tags.
        #[arg(long, conflicts_with = "all")]
        tags: bool,
        /// Use any reference, printed with its category such as `heads/main`.
        #[arg(long)]
        all: bool,
        /// Only use names matching this glob; may be repeated.
        #[arg(long = "match", value_name = "PATTERN")]
        matches: Vec<String>,
        /// Skip names matching this glob; may be repeated.
        #[arg(long = "exclude", value_name = "PATTERN")]
        excludes: Vec<String>,
        /// Consider at most this many tagged ancestors.
        #[arg(long, default_value_t = 10, value_name = "N")]
        candidates: usize,
        /// Only accept a tag on the commit itself.
        #[arg(long)]
        exact_match: bool,
        /// Follow only first parents.
        #[arg(long)]
        first_parent: bool,
        /// Always print the count and ID, even at a tag.
        #[arg(long)]
        long: bool,
        /// Abbreviate the ID to at least this many hex digits; 0 prints only the name.
        #[arg(long, value_name = "N")]
        abbrev: Option<usize>,
        /// Print the abbreviated ID when no name is found.
        #[arg(long)]
        always: bool,
    },
    /// Test whether one commit is an ancestor of another (like `git merge-base --is-ancestor`).
    ///
    /// Exits 0 when it is and 1 when not; prints `yes` or `no` unless `--git` is set.
    Contains {
        /// Possible ancestor.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        ancestor: String,
        /// Possible descendant.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        descendant: String,
    },
}

/// Show commit history (like `git log`).
///
/// Commits are shown newest committer date first, as `git log` does by default. Each commit is a
/// block of `commit`, `parent`, `author` (ISO date) and indented message lines. With `--git`, the
/// output matches `git log`; abbreviated IDs (`Merge:` and `--oneline`) honor `core.abbrev`
/// and lengthen collisions across all stored object kinds.
#[derive(Debug, clap::Args)]
pub struct LogArgs {
    /// Revisions and ranges: `A`, `^A`, `A..B` or `A...B`; `HEAD` when omitted.
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
    revs: Vec<String>,
    /// Show at most this many commits.
    #[arg(short = 'n', long, value_name = "N")]
    limit: Option<usize>,
    /// Resolve author identities using repository mailmap sources.
    #[arg(long, alias = "mailmap", num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    use_mailmap: Option<bool>,
    /// One line per commit: ID and subject.
    #[arg(long)]
    oneline: bool,
    /// Show only commits that change these pathspecs, with Git's default history
    /// simplification: merges that left the paths as one parent had them are hidden and only
    /// that parent is followed.
    #[arg(last = true, value_name = "PATHSPEC")]
    pathspecs: Vec<std::ffi::OsString>,
}

/// Whether tracked files or the index differ from `HEAD`, as `git describe --dirty` asks: staged,
/// unstaged or conflicted changes, with files converted as `girt status` converts them.
///
/// As in Git, untracked files do not count, nor does a submodule whose only change is untracked
/// content, nor an intent-to-add entry whose file was deleted. When converting a file fails (a
/// required clean filter erroring), a raw comparison decides: Git reports such a file dirty from
/// its changed size without running the filter.
fn tracked_changes(repository: &girt::Repository, context: &Context) -> Result<bool> {
    use girt::status::{Baseline, Change, Limits, Report, UntrackedFiles};
    let rules = repository
        .ignore_rules(&context.environment)
        .wrap_err("loading ignore rules")?;
    let mut filters = repository
        .filters(&context.environment, girt::filter::Commands::Run)
        .wrap_err("loading attributes")?;
    let all = girt::pathspec::Pathspec::all();
    let cancel = AtomicBool::new(false);
    let report = match repository.status_with_filters(
        Baseline::Head,
        UntrackedFiles::Omit,
        &rules,
        &all,
        &mut filters,
        Limits::default(),
        &cancel,
    ) {
        Ok(report) => report,
        Err(error) => {
            let raw = repository
                .status_with(
                    Baseline::Head,
                    UntrackedFiles::Omit,
                    &rules,
                    &all,
                    Limits::default(),
                    &cancel,
                )
                .wrap_err("comparing the working tree")?;
            if raw.staged.is_empty() && raw.unstaged.is_empty() && raw.unmerged.is_empty() {
                return Err(error).wrap_err("comparing the working tree");
            }
            return Ok(true);
        }
    };
    let intent_to_add: HashSet<Vec<u8>> = repository
        .read_index(Default::default())
        .wrap_err("reading the index")?
        .map(|index| {
            index
                .entries()
                .iter()
                .filter(|entry| entry.intent_to_add)
                .map(|entry| entry.path.clone())
                .collect()
        })
        .unwrap_or_default();
    let only_untracked = |report: &Report, path: &[u8]| {
        report.submodules.iter().any(|submodule| {
            submodule.path == path
                && submodule.head == Some(submodule.recorded)
                && !submodule.tracked_dirty
        })
    };
    let unstaged = report.unstaged.iter().any(|change| match &change.change {
        Change::Deleted if intent_to_add.contains(&change.path) => false,
        Change::Modified(value)
            if *value == change.index && only_untracked(&report, &change.path) =>
        {
            false
        }
        _ => true,
    });
    Ok(!report.staged.is_empty() || unstaged || !report.unmerged.is_empty())
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let commit = |rev: &str| common::resolve_commit(&repository, &objects, rev);
    let limits = HistoryLimits::trusted();
    let mut out = common::stdout();
    let code = match command {
        Command::Count { revs } => {
            let selection = common::select(&repository, &objects, &revs)?;
            let hidden = hidden(&objects, &selection)?;
            let ids = objects
                .walk_with(&selection.include, limits)
                .wrap_err("walking history")?;
            let count = ids.iter().filter(|id| !hidden.contains(id)).count();
            writeln!(out, "{count}")?;
            ExitCode::SUCCESS
        }
        Command::MergeBase { a, b, all } => {
            let bases = objects
                .merge_bases_with(commit(&a)?, commit(&b)?, limits)
                .wrap_err("finding merge bases")?;
            let shown = if all {
                &bases[..]
            } else {
                &bases[..bases.len().min(1)]
            };
            for base in shown {
                writeln!(out, "{base}")?;
            }
            if bases.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::Describe {
            rev,
            dirty,
            broken,
            tags,
            all,
            matches,
            excludes,
            candidates,
            exact_match,
            first_parent,
            long,
            abbrev,
            always,
        } => {
            use girt::describe::{Candidates, DescribeOptions};
            if (dirty.is_some() || broken.is_some()) && rev.is_some() {
                return Err(eyre!(
                    "--dirty and --broken describe HEAD; they cannot take a revision"
                ));
            }
            let id = commit(rev.as_deref().unwrap_or("HEAD"))?;
            let suffix = if dirty.is_none() && broken.is_none() {
                None
            } else {
                match tracked_changes(&repository, context) {
                    Ok(false) => None,
                    Ok(true) => Some(dirty.clone().unwrap_or_else(|| "-dirty".to_owned())),
                    Err(error) => match &broken {
                        Some(mark) => Some(mark.clone()),
                        None => return Err(error),
                    },
                }
            };
            let mut options = DescribeOptions::new()
                .with_candidates(if exact_match { 0 } else { candidates })
                .with_first_parent(first_parent)
                .with_limits(limits);
            if all {
                options = options.with_candidates_from(Candidates::All);
            } else if tags {
                options = options.with_lightweight_tags();
            }
            for pattern in matches {
                options = options.with_match(pattern);
            }
            for pattern in excludes {
                options = options.with_exclude(pattern);
            }
            if long && abbrev == Some(0) {
                return Err(eyre!("--long and --abbrev=0 cannot be used together"));
            }
            let length = match abbrev {
                Some(length) => length,
                None => common::abbrev_len(&repository, &objects)?,
            };
            let abbreviated = (length != 0)
                .then(|| objects.abbreviate(id, length))
                .transpose()?;
            match repository
                .describe(&objects, id, &options)
                .wrap_err("describing")?
            {
                Some(description) => {
                    if description.renamed {
                        let reference = description.reference.as_bytes();
                        let shown = if all {
                            reference.strip_prefix(b"refs/").unwrap_or(reference)
                        } else {
                            reference.strip_prefix(b"refs/tags/").unwrap_or(reference)
                        };
                        eprintln!(
                            "warning: tag '{}' is externally known as '{}'",
                            display_bytes(shown),
                            display_bytes(
                                description
                                    .name
                                    .strip_prefix(b"tags/")
                                    .filter(|_| all)
                                    .unwrap_or(&description.name)
                            )
                        );
                    }
                    out.write_all(&description.format(abbreviated.as_ref(), long))?;
                    out.write_all(suffix.as_deref().unwrap_or("").as_bytes())?;
                    out.write_all(b"\n")?;
                }
                None if always => {
                    let abbreviated = objects.abbreviate(id, length.max(4))?;
                    writeln!(out, "{abbreviated}{}", suffix.as_deref().unwrap_or(""))?;
                }
                None => return Err(eyre!("no names found to describe {id}")),
            }
            ExitCode::SUCCESS
        }
        Command::Contains {
            ancestor,
            descendant,
        } => {
            let yes = objects
                .is_ancestor_with(commit(&ancestor)?, commit(&descendant)?, limits)
                .wrap_err("checking ancestry")?;
            if !context.git {
                writeln!(out, "{}", if yes { "yes" } else { "no" })?;
            }
            if yes {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    };
    out.flush()?;
    Ok(code)
}

pub fn log(args: LogArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let enabled = args.use_mailmap.unwrap_or(
        repository
            .config()
            .boolean("log", None, "mailmap")?
            .unwrap_or(true),
    );
    let mailmap = if enabled {
        common::mailmap(&repository, &objects)?
    } else {
        girt::mailmap::Mailmap::default()
    };
    let yellow = crate::color::style(context.color, crate::color::YELLOW);
    let reset = crate::color::style(context.color, crate::color::RESET);
    let selection = common::select(&repository, &objects, &args.revs)?;
    let hidden = hidden(&objects, &selection)?;
    let width = common::abbrev_len(&repository, &objects)?;
    let short = |id: ObjectId| objects.abbreviate(id, width).map(|p| p.to_string());
    let mut out = common::stdout();
    let paths = context.pathspec(&repository, &args.pathspecs)?;
    let mut walk = DateOrder::new(&objects, &selection.include, hidden)?;
    let mut first = true;
    let mut shown = 0;
    let cancel = AtomicBool::new(false);
    let limits = TreeCompareLimits::default().with_read(ReadLimits::trusted());
    while let Some((id, commit)) = walk.next_commit()? {
        let parents = if objects.shallow_roots().contains(id) {
            &[][..]
        } else {
            commit.parents()
        };
        // Even `-- .` simplifies history (it hides commits that change nothing), so only a log
        // without pathspecs skips the comparison.
        if args.pathspecs.is_empty() {
            walk.push_all(parents)?;
        } else {
            let step = objects
                .simplify_commit(&commit, parents, &paths, limits, &cancel)
                .wrap_err_with(|| format!("comparing {id} with its parents"))?;
            walk.push_all(&step.follow)?;
            if !step.shown {
                continue;
            }
        }
        shown += 1;
        if args.limit.is_some_and(|limit| shown > limit) {
            break;
        }
        let message = commit.message();
        if args.oneline {
            let id = if context.git {
                short(id)?
            } else {
                id.to_string()
            };
            if context.color {
                writeln!(
                    out,
                    "{yellow}{id}{reset} {}",
                    display_bytes(&subject(message))
                )?;
            } else {
                writeln!(out, "{id} {}", display_bytes(&subject(message)))?;
            }
            continue;
        }
        if !first {
            writeln!(out)?;
        }
        first = false;
        let author = commit
            .author()
            .wrap_err_with(|| format!("parsing author of {id}"))?
            .ok_or_else(|| eyre!("commit {id} has no author"))?;
        let author = mailmap.resolve_signature(author);
        let date = author.date().unwrap_or(IdentityDate {
            seconds: 0,
            offset_minutes: 0,
        });
        if context.git {
            writeln!(out, "commit {id}")?;
            if commit.parents().len() > 1 {
                let parents: Vec<_> = commit
                    .parents()
                    .iter()
                    .map(|p| short(*p))
                    .collect::<Result<_, _>>()?;
                writeln!(out, "Merge: {}", parents.join(" "))?;
            }
            writeln!(out, "Author: {}", identity(author))?;
            // Git's full log substitutes the epoch for malformed or signed seconds,
            // unlike %ad. Valid unsigned dates still fail for a pre-epoch local time.
            let git_date = if author
                .date_bytes
                .trim_ascii_start()
                .first()
                .is_some_and(u8::is_ascii_digit)
                && author.date().is_ok()
            {
                author.format_date(DateFormat::Default)?
            } else {
                IdentityDate {
                    seconds: 0,
                    offset_minutes: 0,
                }
                .format(DateFormat::Default)
            };
            writeln!(out, "Date:   {git_date}")?;
        } else {
            if context.color {
                writeln!(out, "commit {yellow}{id}{reset}")?;
            } else {
                writeln!(out, "commit {id}")?;
            }
            for parent in commit.parents() {
                writeln!(out, "parent {parent}")?;
            }
            writeln!(out, "author {}", identity(author))?;
            writeln!(out, "date   {}", date.format(DateFormat::Iso))?;
        }
        writeln!(out)?;
        for line in message.trim_ascii_end().split(|&b| b == b'\n') {
            if line.is_empty() && !context.git {
                writeln!(out)?;
            } else {
                writeln!(out, "    {}", display_bytes(line))?;
            }
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Git's default `git log` order: a max-heap on committer date, ties in discovery order.
struct DateOrder<'a> {
    objects: &'a Objects,
    heap: BinaryHeap<(i64, std::cmp::Reverse<usize>, ObjectId)>,
    seen: HashSet<ObjectId>,
    pushed: usize,
}

impl<'a> DateOrder<'a> {
    /// Starts at `roots`, never entering `hidden` commits.
    fn new(objects: &'a Objects, roots: &[ObjectId], hidden: HashSet<ObjectId>) -> Result<Self> {
        let mut walk = Self {
            objects,
            heap: BinaryHeap::new(),
            seen: hidden,
            pushed: 0,
        };
        for root in roots {
            walk.push(*root)?;
        }
        Ok(walk)
    }

    fn push(&mut self, id: ObjectId) -> Result<()> {
        if !self.seen.insert(id) {
            return Ok(());
        }
        let commit = self
            .objects
            .read_commit_with(id, ReadLimits::trusted())
            .wrap_err_with(|| format!("reading commit {id}"))?;
        let date = commit
            .committer()
            .ok()
            .flatten()
            .and_then(|committer| committer.date().ok())
            .map_or(0, |date| date.seconds);
        self.heap.push((date, std::cmp::Reverse(self.pushed), id));
        self.pushed += 1;
        Ok(())
    }

    fn push_all(&mut self, ids: &[ObjectId]) -> Result<()> {
        for id in ids {
            self.push(*id)?;
        }
        Ok(())
    }

    /// Pops the newest queued commit; the caller pushes the parents to continue through.
    fn next_commit(&mut self) -> Result<Option<(ObjectId, Commit)>> {
        let Some((_, _, id)) = self.heap.pop() else {
            return Ok(None);
        };
        let commit = self
            .objects
            .read_commit_with(id, ReadLimits::trusted())
            .wrap_err_with(|| format!("reading commit {id}"))?;
        Ok(Some((id, commit)))
    }
}

/// Commits reachable from the selection's excluded side.
///
/// This walks the excluded history in full, unlike Git, which stops once the remaining queue is
/// uninteresting; ranges near the root of a long history pay for the whole excluded side.
fn hidden(objects: &Objects, selection: &Selection) -> Result<HashSet<ObjectId>> {
    if selection.exclude.is_empty() {
        return Ok(HashSet::new());
    }
    let ids = objects
        .walk_with(&selection.exclude, HistoryLimits::trusted())
        .wrap_err("walking excluded history")?;
    Ok(ids.into_iter().collect())
}

/// The first paragraph of a message joined onto one line, like Git's `%s`.
fn subject(message: &[u8]) -> Vec<u8> {
    let mut lines = message
        .split(|&b| b == b'\n')
        .map(<[u8]>::trim_ascii)
        .skip_while(|line| line.is_empty())
        .take_while(|line| !line.is_empty());
    let mut subject = lines.next().unwrap_or_default().to_vec();
    for line in lines {
        subject.push(b' ');
        subject.extend_from_slice(line);
    }
    subject
}

fn identity(identity: IdentityRef<'_>) -> String {
    format!(
        "{} <{}>",
        display_bytes(identity.name),
        display_bytes(identity.email)
    )
}
