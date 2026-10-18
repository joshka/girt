//! Structured blame ranges, deliberately independent of Git's human display format.
use std::io::Write;
use std::process::ExitCode;

use color_eyre::eyre::{Result, bail};

use crate::{Context, common};

/// Attribute file lines to their originating commits.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Literal repository-relative file path.
    path: String,
    /// Include the canonical author email for each range.
    #[arg(short = 'e', long)]
    email: bool,
    /// Revision to attribute.
    #[arg(default_value = "HEAD")]
    revision: String,
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    if context.git {
        bail!("blame has structured range output; --git is unsupported");
    }
    let handle = girt::Repo::new(context.repository()?);
    let repository = handle.repository();
    let objects = repository.objects()?;
    let mailmap = if args.email {
        common::mailmap(repository, &objects)?
    } else {
        girt::mailmap::Mailmap::default()
    };
    let mut emails = std::collections::HashMap::new();
    let mut out = common::stdout();
    for hunk in handle.blame(args.revision, args.path.as_bytes())? {
        if args.email {
            let email = match emails.entry(hunk.commit) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let commit = objects.read_commit(hunk.commit)?;
                    let author = commit
                        .author()?
                        .ok_or_else(|| color_eyre::eyre::eyre!("commit has no author"))?;
                    let author = mailmap.resolve_signature(author);
                    entry.insert(author.email.to_vec())
                }
            };
            write!(out, "<{}> ", common::display_bytes(email))?;
        }
        writeln!(
            out,
            "{} {} {} {} {}",
            hunk.commit,
            hunk.final_start,
            hunk.line_count,
            hunk.original_start,
            common::display_bytes(&hunk.original_path)
        )?;
    }
    Ok(ExitCode::SUCCESS)
}
