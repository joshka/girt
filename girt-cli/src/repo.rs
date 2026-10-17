//! Placeholder for the `repo` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Args)]
pub struct InitArgs {}

pub fn run(_: Command, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn init(_: InitArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
