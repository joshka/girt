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
    /// Revision to attribute.
    #[arg(default_value = "HEAD")]
    revision: String,
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    if context.git {
        bail!("blame has structured range output; --git is unsupported");
    }
    let handle = girt::trusted::Handle::new(context.repository()?);
    let mut out = common::stdout();
    for hunk in handle.blame(args.revision, args.path.as_bytes())? {
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
