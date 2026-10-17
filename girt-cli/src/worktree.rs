//! Placeholder for the `worktree` commands.

use std::process::ExitCode;

use crate::Context;

#[derive(Debug, clap::Subcommand)]
pub enum IndexCommand {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Subcommand)]
pub enum IgnoreCommand {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Subcommand)]
pub enum WorktreeCommand {
    /// Not yet implemented.
    Todo,
}

#[derive(Debug, clap::Args)]
pub struct StatusArgs {}

#[derive(Debug, clap::Args)]
pub struct CheckoutArgs {}

pub fn index(_: IndexCommand, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn status(_: StatusArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn ignore(_: IgnoreCommand, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn checkout(_: CheckoutArgs, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}

pub fn worktree(_: WorktreeCommand, _: &Context) -> anyhow::Result<ExitCode> {
    anyhow::bail!("not yet implemented")
}
