//! Placeholder for the `maintenance` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Subcommand)]
pub enum PackCommand {
    /// Not yet implemented.
    Todo,
}

pub fn run(_: Command, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn pack(_: PackCommand, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
