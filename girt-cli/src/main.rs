//! `girt`: inspect, compare and benchmark Git repositories with the girt library.
//!
//! Commands are grouped by Git concept (`object`, `ref`, `history`, ...). Default output is meant
//! for people and simple scripts; `--git` switches a command to the byte-for-byte output of its
//! Git equivalent so the two tools can be diffed.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};
use color_eyre::eyre::eyre;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod blame;
mod color;
mod common;
mod complete;
mod config;
mod diff;
mod git_hints;
mod history;
mod maintenance;
mod object;
mod progress;
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
    /// Log more detail to stderr (`-v` info, `-vv` debug, `-vvv` trace); `GIRT_LOG` overrides.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "PATH")]
    chdir: Option<PathBuf>,

    /// Print exactly what the equivalent Git command prints.
    #[arg(long, global = true)]
    git: bool,

    /// When to use color; auto honors NO_COLOR, CLICOLOR_FORCE, CLICOLOR and terminal detection.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: color::Color,

    /// Suppress terminal progress on stderr.
    #[arg(short, long, global = true)]
    quiet: bool,

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
    /// Copy a repository from another location and check out HEAD.
    ///
    /// Supports local paths, file URLs, configured HTTP(S), and system OpenSSH on macOS/Linux.
    /// SSH accepts `ssh://[user@]host[:port]/path` and `[user@]host:path`. Normal SSH config,
    /// GIT_SSH_COMMAND, core.sshCommand and GIT_SSH apply. Use an agent or askpass; terminal
    /// prompts and job control are unavailable. HTTP uses configured credential helpers.
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

    /// Attribute file lines (commit, final start, count, original start, path).
    Blame(blame::Args),

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
    ///
    /// Supports local, HTTP(S) credential helpers, and system OpenSSH remotes on macOS/Linux.
    /// Interactive credential and SSH terminal prompts are unavailable. Tags are not followed
    /// automatically. Uses the remote's configured fetch refspecs.
    Fetch(transfer::FetchArgs),
    /// Publish refs to a remote.
    ///
    /// Supports local, HTTP(S) credential helpers, and system OpenSSH remotes on macOS/Linux.
    /// Interactive credential and SSH terminal prompts are unavailable. Updates require the
    /// remote-tracking value to match; fetch first after a stale rejection. Prefix a refspec
    /// with `+` to permit non-fast-forward updates.
    Push(transfer::PushArgs),

    /// Verify and rebuild packs.
    #[command(subcommand)]
    Pack(maintenance::PackCommand),
    /// Plan retention, repack and run maintenance.
    #[command(subcommand)]
    Maintenance(maintenance::Command),

    /// Print a shell completion script; add `source <(girt completions zsh)` to `~/.zshrc`.
    ///
    /// The script calls back into `girt` on each Tab, so it completes revisions, remotes, worktrees
    /// and config keys from the current repository as well as commands and options (the same script
    /// `COMPLETE=<shell> girt` prints). Bash: `source <(girt completions bash)` in `~/.bashrc`.
    /// Fish: `girt completions fish | source` in `~/.config/fish/config.fish`. `--static` prints a
    /// self-contained script without repository values, for packagers.
    Completions(complete::CompletionsArgs),
    /// Print the `girt.1` man page, or write every page with `--out`.
    Man(complete::ManArgs),
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
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();
    let args: Vec<_> = std::env::args_os().collect();
    let early_color = color::Color::early(&args);
    // Help uses stdout; parse errors use stderr. Resolve each before clap prints and exits.
    let mut command = Cli::command();
    let matches = match command.try_get_matches_from_mut(&args) {
        Ok(matches) => matches,
        Err(mut error) => {
            git_hints::annotate(&mut error, &args);
            let enabled = if error.use_stderr() {
                early_color.stderr()
            } else {
                early_color.stdout()
            };
            let choice = if enabled {
                anstream::ColorChoice::Always
            } else {
                anstream::ColorChoice::Never
            };
            choice.write_global();
            let command = command.color(if enabled {
                clap::ColorChoice::Always
            } else {
                clap::ColorChoice::Never
            });
            error.with_cmd(&command).exit();
        }
    };
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    let stderr_color = cli.color.stderr() && !cli.git;
    // Future anstream diagnostics must share the reporting policy too.
    if stderr_color {
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    }
    .write_global();
    if let Err(error) = install_reporting(cli.verbose, stderr_color) {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    if let Some(directory) = &cli.chdir
        && let Err(error) = std::env::set_current_dir(directory)
    {
        eprintln!("error: cannot change to {}: {error}", directory.display());
        return ExitCode::FAILURE;
    }
    let mut context = Context::new(cli.git);
    context.color = cli.color.stdout() && !cli.git;
    context.progress = !cli.quiet && !cli.git;
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
///
/// `GIRT_LOG` (tracing `EnvFilter` directives) overrides the `-v` level; `RUST_LOG` is ignored.
fn install_reporting(verbose: u8, color: bool) -> color_eyre::Result<()> {
    let theme = if color {
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
    // `GIRT_LOG` rather than `RUST_LOG`, so filters meant for other tools don't apply to girt.
    let filter = match std::env::var("GIRT_LOG") {
        Ok(directives) => EnvFilter::try_new(&directives)
            .map_err(|error| eyre!("invalid GIRT_LOG {directives:?}: {error}"))?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new(default),
        Err(error) => return Err(eyre!("invalid GIRT_LOG: {error}")),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(color),
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
        Command::Blame(args) => blame::run(args, context),
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
        Command::Completions(args) => complete::completions(args),
        Command::Man(args) => complete::man(args),
    }
}

fn is_broken_pipe(error: &color_eyre::Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}
