//! Placeholder for the `history` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Args)]
pub struct LogArgs {}

pub fn run(_: Command, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn log(_: LogArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
