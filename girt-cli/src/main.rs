//! `girt`: inspect, compare and benchmark Git repositories with the girt library.
//!
//! Commands are grouped by Git concept (`object`, `ref`, `history`, ...). Default output is meant
//! for people and simple scripts; `--git` switches a command to the byte-for-byte output of its
//! Git equivalent so the two tools can be diffed.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod common;
mod config;
mod diff;
mod history;
mod maintenance;
mod object;
mod refs;
mod repo;
mod transfer;
mod tree;
mod worktree;

pub use common::Context;

/// Inspect, compare and benchmark Git repositories with girt.
#[derive(Debug, Parser)]
#[command(name = "girt", version, about, propagate_version = true)]
struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "PATH")]
    chdir: Option<PathBuf>,

    /// Print exactly what the equivalent Git command prints.
    #[arg(long, global = true)]
    git: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Describe the repository: format, layout, paths and state.
    #[command(subcommand)]
    Repo(repo::Command),
    /// Create a new repository.
    Init(repo::InitArgs),
    /// Copy a repository from another location (without checkout).
    Clone(transfer::CloneArgs),

    /// Hash, read and inspect objects.
    #[command(subcommand)]
    Object(object::Command),
    /// Inspect trees.
    #[command(subcommand)]
    Tree(tree::Command),

    /// List, read and change references.
    #[command(subcommand, name = "ref")]
    Ref(refs::Command),

    /// Show commit history.
    Log(history::LogArgs),
    /// Query commit ancestry.
    #[command(subcommand)]
    History(history::Command),

    /// Compare two trees.
    Diff(diff::DiffArgs),

    /// Inspect the index.
    #[command(subcommand)]
    Index(worktree::IndexCommand),
    /// Show staged, unstaged and untracked changes (raw byte comparison).
    Status(worktree::StatusArgs),
    /// Explain ignore decisions for paths.
    #[command(subcommand)]
    Ignore(worktree::IgnoreCommand),
    /// Replace the index and working tree with a tree (HEAD does not move).
    Checkout(worktree::CheckoutArgs),
    /// Manage linked worktrees.
    #[command(subcommand)]
    Worktree(worktree::WorktreeCommand),

    /// Read and edit configuration.
    #[command(subcommand)]
    Config(config::Command),
    /// Manage and query remotes.
    #[command(subcommand)]
    Remote(config::RemoteCommand),
    /// Download objects and refs from a remote.
    Fetch(transfer::FetchArgs),
    /// Publish refs to a remote.
    Push(transfer::PushArgs),

    /// Verify and rebuild packs.
    #[command(subcommand)]
    Pack(maintenance::PackCommand),
    /// Plan retention, repack and run maintenance.
    #[command(subcommand)]
    Maintenance(maintenance::Command),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let context = Context::new(cli.chdir, cli.git);
    match run(cli.command, &context) {
        Ok(code) => code,
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, context: &Context) -> anyhow::Result<ExitCode> {
    match command {
        Command::Repo(command) => repo::run(command, context),
        Command::Init(args) => repo::init(args, context),
        Command::Clone(args) => transfer::clone(args, context),
        Command::Object(command) => object::run(command, context),
        Command::Tree(command) => tree::run(command, context),
        Command::Ref(command) => refs::run(command, context),
        Command::Log(args) => history::log(args, context),
        Command::History(command) => history::run(command, context),
        Command::Diff(args) => diff::run(args, context),
        Command::Index(command) => worktree::index(command, context),
        Command::Status(args) => worktree::status(args, context),
        Command::Ignore(command) => worktree::ignore(command, context),
        Command::Checkout(args) => worktree::checkout(args, context),
        Command::Worktree(command) => worktree::worktree(command, context),
        Command::Config(command) => config::run(command, context),
        Command::Remote(command) => config::remote(command, context),
        Command::Fetch(args) => transfer::fetch(args, context),
        Command::Push(args) => transfer::push(args, context),
        Command::Pack(command) => maintenance::pack(command, context),
        Command::Maintenance(command) => maintenance::run(command, context),
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}
