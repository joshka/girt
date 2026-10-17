//! `girt log` and `girt history`: commit history walks and ancestry queries.

use std::collections::{BinaryHeap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _, eyre};
use girt::revision::Selection;
use girt::{Commit, HistoryLimits, IdentityRef, ObjectId, Objects};

use crate::Context;
use crate::common::{self, display_bytes};

/// History queries.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Count commits reachable from revisions (like `git rev-list --count`).
    Count {
        /// Revisions and ranges: `A`, `^A`, `A..B` or `A...B`.
        #[arg(required = true)]
        revs: Vec<String>,
    },
    /// Print the best common ancestor of two commits (like `git merge-base`).
    ///
    /// Exits with status 1 when the commits share no history.
    MergeBase {
        /// First commit.
        a: String,
        /// Second commit.
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
        ancestor: String,
        /// Possible descendant.
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
    let objects = common::objects(&repository)?;
    let commit = |rev: &str| common::resolve_commit(&repository, &objects, rev);
    let limits = HistoryLimits::trusted();
    let mut out = common::stdout();
    let code = match command {
        Command::Count { revs } => {
            let selection = common::select(&repository, &objects, &revs)?;
            let hidden = hidden(&objects, &selection)?;
            let ids = objects
                .walk(&selection.include, limits)
                .wrap_err("walking history")?;
            let count = ids.iter().filter(|id| !hidden.contains(id)).count();
            writeln!(out, "{count}")?;
            ExitCode::SUCCESS
        }
        Command::MergeBase { a, b, all } => {
            let bases = objects
                .merge_bases(commit(&a)?, commit(&b)?, limits)
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
                .is_ancestor(commit(&ancestor)?, commit(&descendant)?, limits)
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
    let objects = common::objects(&repository)?;
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
        let (seconds, offset) = match author.date() {
            Ok(date) => (date.seconds, date.offset_minutes),
            Err(_) => (0, 0),
        };
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
            writeln!(out, "Date:   {}", format_date(seconds, offset, false))?;
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
            writeln!(out, "date   {}", format_date(seconds, offset, true))?;
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
        let commit = common::read_commit(self.objects, id)?;
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
        let commit = common::read_commit(self.objects, id)?;
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
        .walk(&selection.exclude, HistoryLimits::trusted())
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

/// Formats a date in its own offset: ISO-like when `iso`, otherwise Git's default format.
fn format_date(seconds: i64, offset_minutes: i16, iso: bool) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local = seconds + i64::from(offset_minutes) * 60;
    let days = local.div_euclid(86_400);
    let time = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (time / 3600, time / 60 % 60, time % 60);
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let offset = offset_minutes.unsigned_abs();
    let zone = format!("{sign}{:02}{:02}", offset / 60, offset % 60);
    if iso {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {zone}")
    } else {
        let weekday = DAYS[usize::try_from(days.rem_euclid(7)).unwrap_or(0)];
        let month = MONTHS[usize::try_from(month - 1).unwrap_or(0)];
        format!("{weekday} {month} {day} {hour:02}:{minute:02}:{second:02} {year} {zone}")
    }
}

/// Converts days since 1970-01-01 to a proleptic Gregorian (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
