//! Placeholder for the `transfer` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Args)]
pub struct CloneArgs {}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {}

#[derive(Debug, clap::Args)]
pub struct PushArgs {}

pub fn clone(_: CloneArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn fetch(_: FetchArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn push(_: PushArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
