//! Placeholder for the `diff` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Args)]
pub struct DiffArgs {}

pub fn run(_: DiffArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
