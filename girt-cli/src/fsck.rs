//! Whole-repository integrity reporting and additive dangling-object recovery.
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap::Args as ClapArgs;
use color_eyre::Result;
use color_eyre::eyre::eyre;
use girt::fsck::{Finding, FindingDetail, MessageId, ObjectFinding, Options, Severity};

use crate::Context;
use crate::common::stdout;

/// Choices for an integrity scan.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Promote default warnings to errors.
    #[arg(long)]
    strict: bool,
    /// Check graph connectivity without payload convention diagnostics.
    #[arg(long)]
    connectivity_only: bool,
    /// Print every unreachable object instead of only dangling tips.
    #[arg(long)]
    unreachable: bool,
    /// Print dangling tips (the default).
    #[arg(long, conflicts_with = "no_dangling")]
    dangling: bool,
    /// Suppress dangling-tip output.
    #[arg(long)]
    no_dangling: bool,
    /// Exclude reflog roots.
    #[arg(long)]
    no_reflogs: bool,
    /// Export dangling objects to .git/lost-found without overwriting conflicting files.
    #[arg(long)]
    lost_found: bool,
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let mut options = Options::default();
    options.strict = args.strict;
    options.connectivity_only = args.connectivity_only;
    options.no_reflogs = args.no_reflogs || args.lost_found;
    options.skip_list_base = Some(std::env::current_dir()?);
    let report = repository.fsck_with(&options, &AtomicBool::new(false))?;
    let missing_kinds: std::collections::BTreeMap<_, _> = report
        .findings
        .iter()
        .filter_map(|finding| match finding {
            ObjectFinding::BrokenLink {
                target, expected, ..
            } => Some((*target, *expected)),
            _ => None,
        })
        .collect();
    let mut out = stdout();
    for finding in &report.findings {
        match finding {
            ObjectFinding::Payload { id, finding } => {
                let kind = report
                    .object_kinds
                    .get(id)
                    .ok_or_else(|| eyre!("integrity report has no kind for {id}"))?
                    .as_str();
                let severity = match finding.severity {
                    Severity::Error | Severity::Fatal => "error",
                    _ => "warning",
                };
                eprintln!(
                    "{severity} in {kind} {id}: {}: {}",
                    finding.message_id.as_str(),
                    description(finding)
                );
            }
            ObjectFinding::BrokenLink {
                source,
                source_kind,
                target,
                expected,
            } => {
                writeln!(out, "broken link from {:>7} {source}", source_kind.as_str())?;
                writeln!(out, "              to {:>7} {target}", expected.as_str())?;
            }
            ObjectFinding::Missing { id } => writeln!(
                out,
                "missing {} {id}",
                missing_kinds.get(id).map_or("object", |kind| kind.as_str())
            )?,
            ObjectFinding::WrongKind {
                id,
                expected,
                actual,
            } => eprintln!(
                "error: object {id} is {}, expected {}",
                actual.as_str(),
                expected.as_str()
            ),
            ObjectFinding::Read { id, source } => eprintln!("error: object {id}: {source}"),
            _ => eprintln!("error: {finding:?}"),
        }
    }
    let ids = if args.unreachable {
        &report.unreachable
    } else {
        &report.dangling
    };
    if args.unreachable || args.dangling || !args.no_dangling {
        for &id in ids {
            let kind = report
                .object_kinds
                .get(&id)
                .ok_or_else(|| eyre!("integrity report has no kind for {id}"))?;
            writeln!(
                out,
                "{} {} {id}",
                if args.unreachable {
                    "unreachable"
                } else {
                    "dangling"
                },
                kind.as_str()
            )?;
        }
    }
    if args.lost_found
        && let Err(error) = repository.write_lost_found(&report)
    {
        for path in &error.written {
            eprintln!("recovered {}", path.display());
        }
        return Err(error.into());
    }
    Ok(if report.has_errors() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn description(finding: &Finding) -> std::borrow::Cow<'_, str> {
    if finding.message_id == MessageId::NulInHeader
        && let Some(FindingDetail::ByteOffset(offset)) = &finding.detail
    {
        return format!("unterminated header: NUL at offset {offset}").into();
    }
    let description = match finding.message_id {
        MessageId::HasDotgit => "contains '.git'",
        MessageId::HasDot => "contains '.'",
        MessageId::HasDotdot => "contains '..'",
        MessageId::DuplicateEntries => "contains duplicate file entries",
        MessageId::TreeNotSorted => "not properly sorted",
        MessageId::BadFilemode => "contains bad file modes",
        MessageId::ZeroPaddedFilemode => "contains zero-padded file modes",
        MessageId::NullSha1 => "contains entries pointing to null sha1",
        MessageId::EmptyName => "contains empty pathname",
        MessageId::FullPathname => "contains full pathnames",
        MessageId::MultipleAuthors => "invalid format - multiple 'author' lines",
        MessageId::NulInCommit => "NUL byte in the commit object body",
        MessageId::UnterminatedHeader => "unterminated header",
        MessageId::MissingAuthor => "invalid format - expected 'author' line",
        MessageId::MissingCommitter => "invalid format - expected 'committer' line",
        MessageId::MissingTaggerEntry => "invalid format - expected 'tagger' line",
        MessageId::BadTree => "cannot be parsed",
        MessageId::MissingEmail => "invalid author/committer line - missing email",
        MessageId::BadEmail => "invalid author/committer line - bad email",
        MessageId::MissingNameBeforeEmail | MessageId::MissingSpaceBeforeEmail => {
            "invalid author/committer line - missing space before email"
        }
        MessageId::MissingSpaceBeforeDate => {
            "invalid author/committer line - missing space before date"
        }
        MessageId::ZeroPaddedDate => "invalid author/committer line - zero-padded date",
        MessageId::BadDateOverflow => {
            "invalid author/committer line - date causes integer overflow"
        }
        MessageId::BadTimezone => "invalid author/committer line - bad time zone",
        MessageId::BadDate => "invalid author/committer line - bad date",
        MessageId::BadName => "invalid author/committer line - bad name",
        _ => "invalid object payload",
    };
    description.into()
}
