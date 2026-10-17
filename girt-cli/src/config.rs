//! Placeholder for the `config` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Subcommand)]
pub enum RemoteCommand {
    /// Not yet implemented.
    Todo,
}

pub fn run(_: Command, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn remote(_: RemoteCommand, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
