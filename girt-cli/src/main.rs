//! `girt`: inspect, compare and benchmark Git repositories with the girt library.
//!
//! Commands are grouped by Git concept (`object`, `ref`, `history`, ...). Default output is meant
//! for people and simple scripts; `--git` switches a command to the byte-for-byte output of its
//! Git equivalent so the two tools can be diffed.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{ArgAction, Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

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
#[command(name = "girt", version, about, propagate_version = true, styles = STYLES)]
struct Cli {
    /// Log more detail to stderr (`-v` info, `-vv` debug, `-vvv` trace); `RUST_LOG` overrides.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

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

/// Help colors in the style of Cargo.
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().bold())
    .valid(AnsiColor::Cyan.on_default().bold())
    .invalid(AnsiColor::Yellow.on_default().bold());

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = install_reporting(cli.verbose) {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    if let Some(directory) = &cli.chdir
        && let Err(error) = std::env::set_current_dir(directory)
    {
        eprintln!("error: cannot change to {}: {error}", directory.display());
        return ExitCode::FAILURE;
    }
    let context = Context::new(cli.git);
    match run(cli.command, &context) {
        Ok(code) => code,
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

/// Installs color-eyre error reports and a stderr tracing subscriber whose spans are captured in
/// those reports.
fn install_reporting(verbose: u8) -> color_eyre::Result<()> {
    let theme = if std::io::stderr().is_terminal() {
        color_eyre::config::Theme::dark()
    } else {
        color_eyre::config::Theme::new()
    };
    color_eyre::config::HookBuilder::default()
        .theme(theme)
        .display_env_section(false)
        .display_location_section(false)
        .install()?;
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .with(tracing_error::ErrorLayer::default())
        .try_init()?;
    Ok(())
}

#[tracing::instrument(level = "debug", skip(context))]
fn run(command: Command, context: &Context) -> color_eyre::Result<ExitCode> {
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

fn is_broken_pipe(error: &color_eyre::Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}
