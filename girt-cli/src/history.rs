//! `girt log` and `girt history`: commit history walks and ancestry queries.

use std::collections::{BinaryHeap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _, eyre};
use girt::display::DateFormat;
use girt::revision::Selection;
use girt::{Commit, HistoryLimits, IdentityDate, IdentityRef, ObjectId, Objects, ReadLimits};

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
    /// One line per commit: ID and subject.
    #[arg(long)]
    oneline: bool,
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
    let yellow = crate::color::style(context.color, crate::color::YELLOW);
    let reset = crate::color::style(context.color, crate::color::RESET);
    let selection = common::select(&repository, &objects, &args.revs)?;
    let hidden = hidden(&objects, &selection)?;
    let width = common::abbrev_len(&repository, &objects)?;
    let short = |id: ObjectId| objects.abbreviate(id, width).map(|p| p.to_string());
    let mut out = common::stdout();
    let mut walk = DateOrder::new(&objects, &selection.include, hidden)?;
    let mut first = true;
    while let Some((id, commit)) = walk.next_commit()? {
        if args.limit.is_some_and(|limit| walk.emitted > limit) {
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
    emitted: usize,
}

impl<'a> DateOrder<'a> {
    /// Starts at `roots`, never entering `hidden` commits.
    fn new(objects: &'a Objects, roots: &[ObjectId], hidden: HashSet<ObjectId>) -> Result<Self> {
        let mut walk = Self {
            objects,
            heap: BinaryHeap::new(),
            seen: hidden,
            pushed: 0,
            emitted: 0,
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

    fn next_commit(&mut self) -> Result<Option<(ObjectId, Commit)>> {
        let Some((_, _, id)) = self.heap.pop() else {
            return Ok(None);
        };
        let commit = self
            .objects
            .read_commit_with(id, ReadLimits::trusted())
            .wrap_err_with(|| format!("reading commit {id}"))?;
        if !self.objects.shallow_roots().contains(id) {
            for parent in commit.parents() {
                self.push(*parent)?;
            }
        }
        self.emitted += 1;
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
