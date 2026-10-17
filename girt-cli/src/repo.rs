//! `girt repo` and `girt init`: describe and create repositories.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _};
use girt::refs::Backend;
use girt::{InitKind, InitOptions, ObjectFormat, OperationLimits, Repository};

use crate::Context;
use crate::common::stdout;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Show object format, reference backend, layout paths, shallowness and operation state.
    ///
    /// Default output is one `key: value` line per field. With `--git`, prints exactly what
    /// `git rev-parse --git-dir --git-common-dir --show-toplevel --is-bare-repository
    /// --show-object-format` prints (without `--show-toplevel` for bare repositories, where Git
    /// would fail; inside a non-bare Git directory it stops after the common directory with Git's
    /// `fatal: this operation must be run in a work tree` and exit status 128). Relative paths
    /// follow Git's observed rules: `.` inside the Git directory, `.git` at the top of the
    /// worktree, `../`-relative common directory from worktree subdirectories, absolute
    /// otherwise.
    Info,
}

/// Create an empty repository, like `git init`.
///
/// The initial branch is `--initial-branch`, else the system or global `init.defaultBranch`, else
/// `master`. Unlike Git, templates are never read. With `--git`, prints Git's
/// `Initialized empty Git repository in <git-dir>/` line.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Directory to create; it must not exist yet.
    directory: Option<PathBuf>,
    /// Create a bare repository with no worktree.
    #[arg(long)]
    bare: bool,
    /// Use SHA-256 object IDs instead of SHA-1.
    #[arg(long)]
    sha256: bool,
    /// Name of the unborn initial branch.
    #[arg(short = 'b', long, value_name = "NAME")]
    initial_branch: Option<String>,
    /// Store references in reftables instead of loose files.
    #[arg(long)]
    reftable: bool,
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    match command {
        Command::Info => info(context),
    }
}

fn info(context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let format = match repository.object_format() {
        ObjectFormat::Sha256 => "sha256",
        _ => "sha1",
    };
    let mut out = stdout();
    if context.git {
        let cwd = canonical(context.directory())?;
        let git_dir = canonical(repository.git_dir())?;
        let common_dir = canonical(repository.common_dir())?;
        let worktree = repository.worktree().map(canonical).transpose()?;
        writeln!(
            out,
            "{}",
            git_dir_display(&cwd, &git_dir, worktree.as_deref())
        )?;
        writeln!(
            out,
            "{}",
            common_dir_display(&cwd, &git_dir, &common_dir, worktree.as_deref())
        )?;
        if let Some(worktree) = &worktree {
            if cwd.starts_with(&git_dir) {
                out.flush()?;
                eprintln!("fatal: this operation must be run in a work tree");
                return Ok(ExitCode::from(128));
            }
            writeln!(out, "{}", worktree.display())?;
        }
        writeln!(out, "{}", repository.is_bare())?;
        writeln!(out, "{format}")?;
    } else {
        let backend = match repository.reference_backend() {
            Backend::Reftable => "reftable",
            _ => "files",
        };
        let operations = repository
            .operation_state(OperationLimits::default())
            .wrap_err("reading operation state")?;
        let roots: Vec<_> = operations.roots().collect();
        writeln!(out, "object-format: {format}")?;
        writeln!(out, "ref-backend: {backend}")?;
        writeln!(out, "bare: {}", repository.is_bare())?;
        writeln!(out, "git-dir: {}", repository.git_dir().display())?;
        writeln!(out, "common-dir: {}", repository.common_dir().display())?;
        writeln!(
            out,
            "object-dir: {}",
            repository.common_dir().join("objects").display()
        )?;
        match repository.worktree() {
            Some(worktree) => writeln!(out, "worktree: {}", worktree.display())?,
            None => writeln!(out, "worktree: none")?,
        }
        writeln!(out, "shallow: {}", !repository.shallow_roots().is_empty())?;
        if roots.is_empty() {
            writeln!(out, "operation: none")?;
        } else {
            writeln!(out, "operation: {}", roots.join(" "))?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn canonical(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .wrap_err_with(|| format!("resolving {}", path.display()))
}

/// `git rev-parse --git-dir`: `.` inside it, `.git` from the worktree top, else absolute.
fn git_dir_display(cwd: &Path, git_dir: &Path, worktree: Option<&Path>) -> String {
    if cwd == git_dir {
        ".".to_owned()
    } else if worktree == Some(cwd) && git_dir == cwd.join(".git") {
        ".git".to_owned()
    } else {
        git_dir.display().to_string()
    }
}

/// `git rev-parse --git-common-dir`: like `--git-dir`, but relative from anywhere in the
/// worktree whose `.git` it is (outside the Git directory itself).
fn common_dir_display(
    cwd: &Path,
    git_dir: &Path,
    common_dir: &Path,
    worktree: Option<&Path>,
) -> String {
    if common_dir == git_dir && cwd == git_dir {
        return ".".to_owned();
    }
    if let Some(worktree) = worktree
        && common_dir == worktree.join(".git")
        && !cwd.starts_with(common_dir)
        && let Ok(relative) = cwd.strip_prefix(worktree)
    {
        let mut path = PathBuf::new();
        for _ in relative.components() {
            path.push("..");
        }
        return path.join(".git").display().to_string();
    }
    common_dir.display().to_string()
}

pub fn init(args: InitArgs, context: &Context) -> Result<ExitCode> {
    let kind = if args.bare {
        InitKind::Bare
    } else {
        InitKind::Worktree
    };
    let config = context.user_config()?;
    let mut options = InitOptions::new(kind);
    if args.initial_branch.is_none() {
        // As in Git, an explicit branch makes `init.defaultBranch` irrelevant, even if invalid.
        options = options
            .defaults_from(&config)
            .wrap_err("invalid init.defaultBranch")?;
    }
    if args.sha256 {
        options = options.with_object_format(ObjectFormat::Sha256);
    }
    if args.reftable {
        options = options.with_reference_backend(Backend::Reftable);
    }
    if let Some(name) = &args.initial_branch {
        options = options
            .with_initial_branch(name)
            .wrap_err_with(|| format!("invalid branch name `{name}`"))?;
    }
    let target = match &args.directory {
        Some(directory) => context.directory().join(directory),
        None => context.directory().to_path_buf(),
    };
    let repository = Repository::init_with_options(&target, &options)
        .wrap_err_with(|| format!("initializing {}", target.display()))?;
    let git_dir = canonical(repository.git_dir())?;
    let mut out = stdout();
    if context.git {
        writeln!(
            out,
            "Initialized empty Git repository in {}/",
            git_dir.display()
        )?;
    } else {
        writeln!(out, "{}", git_dir.display())?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
