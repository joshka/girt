//! `girt bisect`: binary search through history, sharing Git's bisect state.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _, eyre};
use girt::bisect::{Bisect, Mark, ResetTo, Start, Step, Terms, Verdict};
use girt::display::DateFormat;
use girt::{ObjectId, Repository, Signature};

use crate::common::{self, display_bytes};
use crate::{Context, complete};

/// Find the commit that introduced a change by binary search (like `git bisect`).
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Start a bisection, optionally with a new commit, old commits and paths.
    Start {
        /// Word for commits with the change (default `bad`).
        #[arg(long, alias = "term-bad", value_name = "TERM")]
        term_new: Option<String>,
        /// Word for commits without it (default `good`).
        #[arg(long, alias = "term-good", value_name = "TERM")]
        term_old: Option<String>,
        /// Record the commit to test in `BISECT_HEAD` instead of checking it out.
        #[arg(long)]
        no_checkout: bool,
        /// Follow only first parents.
        #[arg(long)]
        first_parent: bool,
        /// A new commit, then old commits.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        revisions: Vec<String>,
        /// Only consider commits that change these paths.
        #[arg(last = true)]
        paths: Vec<String>,
    },
    /// Mark commits as having the change (default: the commit being tested).
    #[command(visible_alias = "bad")]
    New {
        /// Commits to mark.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        revisions: Vec<String>,
    },
    /// Mark commits as not having the change (default: the commit being tested).
    #[command(visible_alias = "good")]
    Old {
        /// Commits to mark.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        revisions: Vec<String>,
    },
    /// Mark commits that cannot be tested; `A..B` skips a range.
    Skip {
        /// Commits or ranges to skip.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        revisions: Vec<String>,
    },
    /// Check out the next commit to test.
    Next,
    /// End the bisection and return to the original branch, or to COMMIT.
    Reset {
        /// Commit to check out instead of the original branch.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        commit: Option<String>,
    },
    /// Print the bisect log.
    Log,
    /// Replay a bisect log.
    Replay {
        /// Log file written by `girt bisect log` or `git bisect log`.
        file: PathBuf,
    },
    /// List the commits still in question, best split first.
    Visualize,
    /// Print the terms in use.
    Terms,
    /// Run a command on each commit until the first new commit is found.
    ///
    /// Exit status 0 marks the commit old, 125 skips it, 1 to 127 mark it new, and 128 or more
    /// stops the run.
    Run {
        /// The command and its arguments.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    // Only commands that move HEAD write reflog entries, so read-only ones need no identity.
    let read_only = matches!(command, Command::Log | Command::Visualize | Command::Terms);
    let committer = match Signature::committer(repository.config(), &context.environment) {
        Ok(committer) => committer,
        Err(_) if read_only => Signature {
            name: Vec::new(),
            email: Vec::new(),
            seconds: 0,
            offset_minutes: 0,
        },
        Err(error) => return Err(error).wrap_err("reading the committer identity"),
    };
    let open = || -> Result<Bisect<'_>> {
        Bisect::open(&repository, committer.clone())?
            .ok_or_else(|| eyre!("no bisection in progress; start one with `girt bisect start`"))
    };

    let step = match command {
        Command::Start {
            term_new,
            term_old,
            no_checkout,
            first_parent,
            revisions,
            paths,
        } => {
            let terms = match (term_new, term_old) {
                (None, None) => Terms::default(),
                (new, old) => Terms::new(
                    new.unwrap_or_else(|| "bad".into()),
                    old.unwrap_or_else(|| "good".into()),
                )?,
            };
            let ids = resolve_all(&repository, &revisions)?;
            let mut start = Start::default()
                .with_terms(terms)
                .with_no_checkout(no_checkout)
                .with_first_parent(first_parent)
                .with_paths(paths.into_iter().map(String::into_bytes).collect());
            if let Some((&new, olds)) = ids.split_first() {
                start = start.with_new(new).with_olds(olds.to_vec());
            }
            let (bisect, step) = Bisect::start(&repository, committer.clone(), start)?;
            report(&repository, &bisect, step)?
        }
        Command::New { revisions } => mark(&repository, open()?, Mark::New, &revisions)?,
        Command::Old { revisions } => mark(&repository, open()?, Mark::Old, &revisions)?,
        Command::Skip { revisions } => {
            let bisect = open()?;
            let ids = if revisions.is_empty() {
                vec![bisect.current()?]
            } else {
                skip_ids(&repository, &revisions)?
            };
            let mut bisect = bisect;
            let step = bisect.mark(Mark::Skip, &ids)?;
            report(&repository, &bisect, step)?
        }
        Command::Next => {
            let mut bisect = open()?;
            let step = bisect.next_step()?;
            report(&repository, &bisect, step)?
        }
        Command::Reset { commit } => {
            let to = match commit {
                Some(commit) => ResetTo::Commit(common::resolve_commit(
                    &repository,
                    &repository.objects()?,
                    &commit,
                )?),
                None => ResetTo::Original,
            };
            match Bisect::open(&repository, committer.clone())? {
                Some(bisect) => bisect.reset(to)?,
                None => println!("We are not bisecting."),
            }
            ExitCode::SUCCESS
        }
        Command::Log => {
            let mut out = common::stdout();
            out.write_all(&open()?.log()?)?;
            out.flush()?;
            ExitCode::SUCCESS
        }
        Command::Replay { file } => {
            let log =
                std::fs::read(&file).wrap_err_with(|| format!("reading {}", file.display()))?;
            let (bisect, step) = Bisect::replay(&repository, committer.clone(), &log)?;
            report(&repository, &bisect, step)?
        }
        Command::Visualize => {
            let bisect = open()?;
            let candidates = bisect.candidates()?;
            let objects = repository.objects()?;
            let mut out = common::stdout();
            for id in candidates.testable() {
                let commit = objects.read_commit(id)?;
                writeln!(out, "{id} {}", display_bytes(&subject(commit.message())))?;
            }
            out.flush()?;
            ExitCode::SUCCESS
        }
        Command::Terms => {
            let bisect = open()?;
            let terms = bisect.terms();
            println!(
                "Your current terms are '{}' for the old state\nand '{}' for the new state.",
                terms.old_term(),
                terms.new_term()
            );
            ExitCode::SUCCESS
        }
        Command::Run { command } => {
            // Git runs the command from the top of the working tree.
            let root = repository
                .worktree()
                .ok_or_else(|| eyre!("bisect run needs a working tree"))?
                .to_path_buf();
            let mut bisect = open()?;
            let step = bisect.run(|commit| {
                println!("running {}", command.join(" "));
                let status = std::process::Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(&root)
                    .status();
                let verdict = match status {
                    Ok(status) => Verdict::from_exit_code(status.code()),
                    Err(error) => {
                        eprintln!("bisect run failed to start {}: {error}", command[0]);
                        Verdict::Abort
                    }
                };
                if verdict == Verdict::Abort {
                    eprintln!("bisect run failed at {commit}");
                }
                verdict
            });
            match step {
                Ok(step) => report(&repository, &bisect, step)?,
                Err(girt::bisect::Error::Aborted(_)) => ExitCode::FAILURE,
                Err(error) => return Err(error.into()),
            }
        }
    };
    Ok(step)
}

fn mark(
    repository: &Repository,
    mut bisect: Bisect<'_>,
    mark: Mark,
    revisions: &[String],
) -> Result<ExitCode> {
    let ids = if revisions.is_empty() {
        vec![bisect.current()?]
    } else {
        resolve_all(repository, revisions)?
    };
    let step = bisect.mark(mark, &ids)?;
    report(repository, &bisect, step)
}

fn resolve_all(repository: &Repository, revisions: &[String]) -> Result<Vec<ObjectId>> {
    let objects = repository.objects()?;
    revisions
        .iter()
        .map(|revision| common::resolve_commit(repository, &objects, revision))
        .collect()
}

/// Commits to skip: single revisions, and `A..B` as every commit in the range.
fn skip_ids(repository: &Repository, revisions: &[String]) -> Result<Vec<ObjectId>> {
    let objects = repository.objects()?;
    let mut ids = Vec::new();
    for revision in revisions {
        if revision.contains("..") {
            let range: girt::revision::Range = revision
                .parse()
                .map_err(|error| eyre!("parsing `{revision}`: {error}"))?;
            let selection =
                girt::revision::Resolver::new(repository, &objects)?.select(&[range])?;
            ids.extend(objects.walk_range(&selection.include, &selection.exclude)?);
        } else {
            ids.push(common::resolve_commit(repository, &objects, revision)?);
        }
    }
    Ok(ids)
}

/// Prints a step as Git does; exit status 2 when only skipped commits remain.
fn report(repository: &Repository, bisect: &Bisect<'_>, step: Step) -> Result<ExitCode> {
    let objects = repository.objects()?;
    let terms = bisect.terms();
    let mut out = common::stdout();
    let subject_of = |id: ObjectId| -> Result<String> {
        Ok(display_bytes(&subject(objects.read_commit(id)?.message())).into_owned())
    };
    let plural = |n: usize, word: &str| {
        if n == 1 {
            format!("{n} {word}")
        } else {
            format!("{n} {word}s")
        }
    };

    let code = match step {
        Step::Waiting { new, olds } => {
            let status = match (new, olds) {
                (false, 0) => format!(
                    "waiting for both '{}' and '{}' commits",
                    terms.old_term(),
                    terms.new_term()
                ),
                (false, olds) => format!(
                    "waiting for '{}' commit, {olds} '{}' commit known",
                    terms.new_term(),
                    terms.old_term()
                ),
                (true, _) => format!(
                    "waiting for '{}' commit(s), '{}' commit known",
                    terms.old_term(),
                    terms.new_term()
                ),
            };
            writeln!(out, "status: {status}")?;
            ExitCode::SUCCESS
        }
        Step::MergeBase(id) => {
            writeln!(out, "Bisecting: a merge base must be tested")?;
            writeln!(out, "[{id}] {}", subject_of(id)?)?;
            ExitCode::SUCCESS
        }
        Step::Test {
            commit,
            remaining,
            steps,
        } => {
            writeln!(
                out,
                "Bisecting: {} left to test after this (roughly {})",
                plural(remaining, "revision"),
                plural(steps, "step")
            )?;
            writeln!(out, "[{commit}] {}", subject_of(commit)?)?;
            ExitCode::SUCCESS
        }
        Step::Found(id) => {
            writeln!(out, "{id} is the first '{}' commit", terms.new_term())?;
            let commit = objects.read_commit(id)?;
            writeln!(out, "commit {id}")?;
            if let Some(author) = commit.author()? {
                writeln!(
                    out,
                    "Author: {} <{}>",
                    display_bytes(author.name),
                    display_bytes(author.email)
                )?;
                writeln!(out, "Date:   {}", author.format_date(DateFormat::Default)?)?;
            }
            writeln!(out)?;
            for line in commit.message().trim_ascii_end().split(|&b| b == b'\n') {
                writeln!(out, "    {}", display_bytes(line))?;
            }
            ExitCode::SUCCESS
        }
        Step::OnlySkipped(possible) => {
            writeln!(out, "There are only 'skip'ped commits left to test.")?;
            writeln!(
                out,
                "The first '{}' commit could be any of:",
                terms.new_term()
            )?;
            for id in possible {
                writeln!(out, "{id}")?;
            }
            writeln!(out, "We cannot bisect more!")?;
            ExitCode::from(2)
        }
        other => return Err(eyre!("unexpected bisect step {other:?}")),
    };
    out.flush()?;
    Ok(code)
}

/// The first paragraph of a message on one line, as Git's `%s` prints it.
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
