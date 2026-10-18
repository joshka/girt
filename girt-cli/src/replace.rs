//! `girt replace`: create, list and delete replace references.

use std::io::Write;
use std::process::ExitCode;

use clap_complete::ArgValueCompleter;
use color_eyre::eyre::{Result, WrapErr as _};
use girt::refs::Target;
use girt::replace::Force;

use crate::common::{self, resolve};
use crate::{Context, complete};

/// Replace references: make one object stand for another when history and objects are read.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Make REPLACEMENT stand for ORIGINAL (like `git replace`).
    ///
    /// Both objects must exist and have the same kind unless `--force` is given.
    Set {
        /// Object to replace.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        original: String,
        /// Object read in its place.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        replacement: String,
        /// Overwrite an existing replace reference and allow a different kind.
        #[arg(short, long)]
        force: bool,
    },
    /// Replace COMMIT with a copy that has PARENTS (like `git replace --graft`).
    ///
    /// The copy drops any signature; with no parents it is a root commit.
    Graft {
        /// Commit to replace.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        commit: String,
        /// Parents of the copy, in order.
        #[arg(add = ArgValueCompleter::new(complete::revisions))]
        parents: Vec<String>,
        /// Overwrite an existing replace reference.
        #[arg(short, long)]
        force: bool,
    },
    /// Delete the replace references of objects (like `git replace -d`).
    Delete {
        /// Replaced objects.
        #[arg(required = true, add = ArgValueCompleter::new(complete::revisions))]
        objects: Vec<String>,
    },
    /// List replaced objects and their replacements (like `git replace --format=short -l`).
    List,
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let force = |force| if force { Force::Yes } else { Force::No };
    let mut out = common::stdout();
    match command {
        Command::Set {
            original,
            replacement,
            force: forced,
        } => {
            let original = resolve(&repository, &original)?;
            let replacement = resolve(&repository, &replacement)?;
            repository
                .replace(original, replacement, force(forced))
                .wrap_err_with(|| format!("replacing {original}"))?;
        }
        Command::Graft {
            commit,
            parents,
            force: forced,
        } => {
            let commit = resolve(&repository, &commit)?;
            let parents = parents
                .iter()
                .map(|parent| resolve(&repository, parent))
                .collect::<Result<Vec<_>>>()?;
            let grafted = repository
                .replace_graft(commit, &parents, force(forced))
                .wrap_err_with(|| format!("grafting {commit}"))?;
            if grafted.dropped_signature {
                eprintln!("warning: the signature of {commit} was removed in its replacement");
            }
            writeln!(out, "{}", grafted.replacement)?;
        }
        Command::Delete { objects } => {
            for object in objects {
                let id = resolve(&repository, &object)?;
                repository
                    .delete_replacement(id)
                    .wrap_err_with(|| format!("deleting the replace reference of {id}"))?;
                writeln!(out, "Deleted replace ref '{id}'")?;
            }
        }
        Command::List => {
            for reference in repository.references()?.list_prefix("refs/replace/")? {
                let name = &reference.name.as_bytes()["refs/replace/".len()..];
                if let Target::Direct(id) = reference.target {
                    out.write_all(name)?;
                    writeln!(out, " {id}")?;
                }
            }
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
