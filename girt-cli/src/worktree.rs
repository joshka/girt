//! Index, status, ignore, checkout and linked-worktree commands.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use clap_complete::{ArgValueCandidates, ArgValueCompleter};
use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::checkout::Action;
use girt::ignore::{InputKind, SourceFile, WorktreeRules};
use girt::index::Stage;
use girt::refs::RefName;
use girt::status::{Baseline, Change, Limits, UntrackedFiles, UntrackedKind, WorktreeChange};
use girt::{ChangeStatus, ObjectId, Repository, WorktreeRetirement, WorktreeState};

use crate::common::{display_bytes, resolve_ref, resolve_tree, stdout, write_path};
use crate::{Context, complete};

/// Maximum registrations listed by `worktree list`.
const MAX_WORKTREES: usize = 100_000;

/// Inspect the index.
#[derive(Debug, clap::Subcommand)]
pub enum IndexCommand {
    /// List index entries (like `git ls-files`).
    ///
    /// Prints one path per line; `--stage` adds `<mode> <id> <stage>` before a tab. With `--git`,
    /// paths are C-quoted like Git's default `core.quotePath`.
    List {
        /// Show mode, object ID and stage number for each entry (like `git ls-files -s`).
        #[arg(short, long)]
        stage: bool,
    },
}

/// Show raw differences between HEAD, the index and the working tree.
///
/// This is a raw byte comparison: no clean/smudge filters, no EOL conversion, no
/// `core.filemode`/`core.symlinks` policy and no rename detection. Default output is one
/// `<XY> <path>` line per path, using the `git status --porcelain` letters. With `--git`, paths
/// are C-quoted and untracked directories are collapsed to `dir/` as `git status --porcelain`
/// does; without `--untracked` it corresponds to `git status --porcelain -uno`. girt currently
/// refuses non-ASCII working-tree paths on macOS.
#[derive(Debug, clap::Args)]
pub struct StatusArgs {
    /// Start with a `## branch...upstream [ahead N, behind M]` line, as `git status -sb` does.
    #[arg(short, long)]
    branch: bool,
    /// Also list untracked files, filtered through .gitignore, info/exclude and
    /// core.excludesFile.
    #[arg(short, long)]
    untracked: bool,
    /// Limit the report to these pathspecs, relative to the current directory: `:/` for the
    /// repository root, `:!` to exclude, `:(icase)`, `:(glob)`.
    #[arg(last = true, value_name = "PATHSPEC")]
    pathspecs: Vec<std::ffi::OsString>,
    #[command(flatten)]
    filters: FilterArgs,
}

/// How working files are converted, as `.gitattributes` and `core.autocrlf` ask.
#[derive(Debug, clap::Args)]
pub struct FilterArgs {
    /// Compare or write raw bytes: no line-ending conversion, `ident` or filter programs.
    #[arg(long)]
    raw: bool,
    /// Convert line endings and `ident`, but fail on a path whose filter would run a program
    /// (such as Git LFS) instead of running it.
    #[arg(long, conflicts_with = "raw")]
    refuse_filter_commands: bool,
}

impl FilterArgs {
    pub fn commands(&self) -> Option<girt::filter::Commands> {
        if self.raw {
            None
        } else if self.refuse_filter_commands {
            Some(girt::filter::Commands::Refuse)
        } else {
            Some(girt::filter::Commands::Run)
        }
    }
}

/// Explain ignore decisions for paths.
#[derive(Debug, clap::Subcommand)]
pub enum IgnoreCommand {
    /// Report the rule deciding each path (like `git check-ignore -v`).
    ///
    /// Paths are repository-relative. Tracked paths are never ignored. Exits 0 when any path is
    /// ignored and 1 otherwise. With `--git`, only paths matching a rule (including `!` negations)
    /// are printed, as `<source>:<line>:<pattern>\t<path>`, and the exit code reflects whether
    /// any path matched; source names assume the command runs at the root.
    Check {
        /// Paths to check, relative to the repository root.
        #[arg(required = true)]
        paths: Vec<String>,
    },
}

/// Replace the index and working tree with a tree; HEAD does not move.
///
/// The baseline must describe the current index and working tree exactly: girt refuses dirty,
/// staged or conflicted paths rather than overwriting them. Files are converted as Git writes
/// them (line endings, `ident`, filter programs such as Git LFS) unless `--raw` is given; progress
/// is shown only for raw checkouts. Afterwards `git status` may report staged changes relative to
/// HEAD.
///
/// Derived index caches (`TREE`, `FSMN`, `UNTR`, `EOIE`, `IEOT`) are dropped for Git to rebuild.
/// Split/sparse indexes, resolve-undo data and unknown extensions are refused.
#[derive(Debug, clap::Args)]
pub struct CheckoutArgs {
    /// Revision whose tree to check out.
    #[arg(add = ArgValueCompleter::new(complete::revisions))]
    revision: String,
    /// Revision whose tree the current index represents; an unborn HEAD or a missing index (as in
    /// a fresh `clone --no-checkout`) means empty.
    #[arg(long, default_value = "HEAD", add = ArgValueCompleter::new(complete::revisions))]
    from: String,
    #[command(flatten)]
    filters: FilterArgs,
}

/// Manage linked worktrees.
#[derive(Debug, clap::Subcommand)]
pub enum WorktreeCommand {
    /// List the main worktree and registered linked worktrees (like `git worktree list`).
    ///
    /// Default output is `<path> <HEAD id> <branch or "(detached)">` with full IDs and a trailing
    /// state for missing or unreadable checkouts. With `--git`, IDs honor `core.abbrev`
    /// and lengthen collisions; `locked`/`prunable`
    /// annotations are not printed.
    List,
    /// Protect a registration from pruning.
    Lock {
        /// Registration name (under `worktrees/`) or checkout path.
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: String,
        /// Reason recorded in the lock.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Remove a registration's lock.
    Unlock {
        /// Registration name (under `worktrees/`) or checkout path.
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: String,
    },
    /// Delete one registration whose checkout no longer exists, including its HEAD and index.
    Prune {
        /// Registration name (under `worktrees/`).
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: String,
    },
    /// Repair the links between a registration and its (moved) checkout.
    Repair {
        /// Registration name (under `worktrees/`).
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: String,
        /// Current checkout directory.
        path: PathBuf,
    },
}

pub fn index(command: IndexCommand, context: &Context) -> Result<ExitCode> {
    let IndexCommand::List { stage } = command;
    let repository = context.repository()?;
    let index = repository
        .read_index(Default::default())
        .wrap_err("reading index")?;
    let mut out = stdout();
    for entry in index.as_ref().map_or(&[][..], |index| index.entries()) {
        if stage {
            write!(
                out,
                "{:06o} {} {}\t",
                entry.mode as u32, entry.id, entry.stage as u16
            )?;
        }
        write_path(&mut out, &entry.path, context.git)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

pub fn status(args: StatusArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let listing = match (args.untracked, context.git) {
        (false, _) => UntrackedFiles::Omit,
        (true, true) => UntrackedFiles::Collapsed,
        (true, false) => UntrackedFiles::All,
    };
    let rules = repository
        .ignore_rules(&context.environment)
        .wrap_err("loading ignore rules")?;
    let paths = context.pathspec(&repository, &args.pathspecs)?;
    let report = match args.filters.commands() {
        None => repository.status_with(
            Baseline::Head,
            listing,
            &rules,
            &paths,
            Limits::default(),
            &AtomicBool::new(false),
        ),
        Some(commands) => {
            let mut filters = repository
                .filters(&context.environment, commands)
                .wrap_err("loading attributes")?;
            repository.status_with_filters(
                Baseline::Head,
                listing,
                &rules,
                &paths,
                &mut filters,
                Limits::default(),
                &AtomicBool::new(false),
            )
        }
    }
    .wrap_err("computing status")?;

    let mut lines: BTreeMap<Vec<u8>, [char; 2]> = BTreeMap::new();
    for change in &report.staged {
        let code = change.status().letter();
        lines.entry(change.path.clone()).or_insert([' '; 2])[0] = code;
    }
    let worktree = repository
        .worktree()
        .ok_or_else(|| eyre!("status needs a worktree"))?;
    for change in &report.unstaged {
        let code = change
            .status()
            .map_or_else(|| obstruction_code(worktree, change), ChangeStatus::letter);
        lines.entry(change.path.clone()).or_insert([' '; 2])[1] = code;
    }
    let mut stages: BTreeMap<&[u8], BTreeSet<Stage>> = BTreeMap::new();
    for entry in &report.unmerged {
        stages.entry(&entry.path).or_default().insert(entry.stage);
    }
    for (path, stages) in stages {
        let has = |stage| stages.contains(&stage);
        let code = match (has(Stage::Base), has(Stage::Ours), has(Stage::Theirs)) {
            (true, false, false) => ['D', 'D'],
            (false, true, false) => ['A', 'U'],
            (true, true, false) => ['U', 'D'],
            (false, false, true) => ['U', 'A'],
            (true, false, true) => ['D', 'U'],
            (false, true, true) => ['A', 'A'],
            _ => ['U', 'U'],
        };
        lines.insert(path.to_vec(), code);
    }

    let untracked = report.untracked.iter().map(|entry| match entry.kind {
        UntrackedKind::File => entry.path.clone(),
        _ => [entry.path.as_slice(), b"/"].concat(),
    });

    let mut out = stdout();
    if args.branch {
        out.write_all(b"## ")?;
        out.write_all(&branch_header(&repository)?)?;
        out.write_all(b"\n")?;
    }
    let reset = crate::color::style(context.color, crate::color::RESET);
    for (path, code) in &lines {
        if context.color {
            let staged = crate::color::GREEN;
            let unstaged = crate::color::RED;
            write!(
                out,
                "{staged}{}{reset}{unstaged}{}{reset}",
                code[0], code[1]
            )?;
        } else {
            write!(out, "{}{}", code[0], code[1])?;
        }
        out.write_all(b" ")?;
        write_path(&mut out, path, context.git)?;
    }
    for path in untracked {
        if context.color {
            write!(out, "{}??{reset} ", crate::color::RED)?;
        } else {
            out.write_all(b"?? ")?;
        }
        write_path(&mut out, &path, context.git)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The branch line of `git status -sb` without its `## ` prefix: the branch, its upstream and how
/// far each is from the other, `[gone]` when the upstream reference is missing. Bytes are kept
/// as stored, as Git prints them. An unborn branch always reports its upstream as gone, as Git
/// does.
fn branch_header(repository: &Repository) -> Result<Vec<u8>> {
    let head = repository.head().wrap_err("reading HEAD")?;
    let Some(branch) = head.name.as_bytes().strip_prefix(b"refs/heads/") else {
        return Ok(b"HEAD (no branch)".to_vec());
    };
    let mut header = match head.id {
        Some(_) => branch.to_vec(),
        None => [b"No commits yet on ".as_slice(), branch].concat(),
    };
    let objects = repository.objects().wrap_err("opening objects")?;
    let resolver = girt::revision::Resolver::new(repository, &objects)?;
    let upstream = match resolver.upstream_name(None) {
        Ok(upstream) => upstream,
        Err(girt::revision::RevisionError::NoUpstream { .. }) => return Ok(header),
        Err(error) => return Err(error).wrap_err("reading the upstream"),
    };
    let short = ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|prefix| upstream.as_bytes().strip_prefix(prefix.as_bytes()))
        .unwrap_or(upstream.as_bytes());
    header.extend_from_slice(b"...");
    header.extend_from_slice(short);
    let references = repository.references().wrap_err("opening references")?;
    let upstream_id = references
        .resolve(&upstream)
        .wrap_err("reading the upstream")?
        .id;
    let (Some(id), Some(upstream_id)) = (head.id, upstream_id) else {
        header.extend_from_slice(b" [gone]");
        return Ok(header);
    };
    let upstream_commit = objects
        .peel(
            upstream_id,
            girt::PeelLimits::default(),
            &AtomicBool::new(false),
        )
        .wrap_err("peeling the upstream")?
        .target;
    let counts = objects
        .ahead_behind(id, upstream_commit)
        .wrap_err("counting commits")?;
    let distance = match (counts.ahead, counts.behind) {
        (0, 0) => String::new(),
        (ahead, 0) => format!(" [ahead {ahead}]"),
        (0, behind) => format!(" [behind {behind}]"),
        (ahead, behind) => format!(" [ahead {ahead}, behind {behind}]"),
    };
    header.extend_from_slice(distance.as_bytes());
    Ok(header)
}

/// Status letter for a changed entry: `T` when the kind changes, otherwise `M`.
/// Git reports a special file occupying a tracked leaf as modified, and a directory there or a
/// blocked ancestor as deleted.
fn obstruction_code(worktree: &Path, change: &WorktreeChange) -> char {
    let Change::Obstructed { path } = &change.change else {
        return 'D';
    };
    #[cfg(unix)]
    let relative = Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path));
    #[cfg(not(unix))]
    let relative = PathBuf::from(String::from_utf8_lossy(path).into_owned());
    let special = *path == change.path
        && std::fs::symlink_metadata(worktree.join(relative))
            .is_ok_and(|metadata| !metadata.is_dir());
    if special { 'M' } else { 'D' }
}

pub fn ignore(command: IgnoreCommand, context: &Context) -> Result<ExitCode> {
    let IgnoreCommand::Check { paths } = command;
    let repository = context.repository()?;
    let root = worktree_root(&repository)?.to_owned();
    let index = repository
        .read_index(Default::default())
        .wrap_err("reading index")?;
    let tracked: HashSet<&[u8]> = index
        .as_ref()
        .map_or(&[][..], |index| index.entries())
        .iter()
        .map(|entry| entry.path.as_slice())
        .collect();
    let mut rules = repository
        .ignore_rules(&context.environment)
        .wrap_err("loading ignore rules")?;
    let mut any = false;
    let mut out = stdout();
    for path in &paths {
        let key = path.trim_end_matches('/').as_bytes();
        let kind = if root.join(path).is_dir() {
            InputKind::Directory
        } else {
            InputKind::File
        };
        let found = if tracked.contains(key) {
            None
        } else {
            rules
                .check(key, kind)
                .wrap_err_with(|| format!("checking {path}"))?
        };
        any |= found.is_some_and(|m| m.ignored || context.git);
        match (found, context.git) {
            (Some(found), true) => {
                let (source, pattern) = describe(&rules, &root, found.source, found.line);
                out.write_all(format!("{source}:{}:", found.line).as_bytes())?;
                out.write_all(pattern)?;
                writeln!(out, "\t{path}")?;
            }
            (_, true) => {}
            (Some(found), false) => {
                let (source, pattern) = describe(&rules, &root, found.source, found.line);
                let verdict = if found.ignored {
                    "ignored"
                } else {
                    "not ignored"
                };
                writeln!(
                    out,
                    "{path}: {verdict} by {source}:{}: {}",
                    found.line,
                    display_bytes(pattern)
                )?;
            }
            (None, false) => writeln!(out, "{path}: not ignored")?,
        }
    }
    out.flush()?;
    Ok(if any {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub fn checkout(args: CheckoutArgs, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let tree = |revision: &str| -> Result<Option<ObjectId>> {
        if revision == "HEAD" {
            let references = repository.references().wrap_err("opening references")?;
            let head = RefName::HEAD;
            if resolve_ref(&references, &head)?.is_none() {
                return Ok(None);
            }
        }
        resolve_tree(&repository, &objects, revision).map(Some)
    };
    let index_exists = repository
        .read_index(Default::default())
        .wrap_err("reading the index")?
        .is_some();
    let baseline = if index_exists {
        tree(&args.from)?
    } else {
        None
    };
    let target = tree(&args.revision)?;
    let mut progress = crate::progress::Progress::new(context.progress);
    let result = match (args.filters.commands(), target) {
        (Some(commands), Some(target)) => {
            let mut current = repository
                .filters(&context.environment, commands)
                .wrap_err("loading attributes")?;
            let mut next = repository
                .filters_for_checkout(&context.environment, target, commands)
                .wrap_err("loading attributes")?;
            repository.checkout_tree_with_filters(
                baseline,
                Some(target),
                &mut current,
                &mut next,
                Default::default(),
                &AtomicBool::new(false),
            )
        }
        _ => repository.checkout_tree_with_progress(
            baseline,
            target,
            Default::default(),
            &AtomicBool::new(false),
            |done, total| progress.update("Updating files", done, Some(total)),
        ),
    };
    let report = match result {
        Ok(report) => report,
        Err(failure) => {
            let applied = failure.report.applied.len();
            return Err(color_eyre::eyre::Report::new(failure))
                .wrap_err_with(|| format!("checkout failed after {applied} operations"));
        }
    };
    progress.finish();
    if context.git {
        return Ok(ExitCode::SUCCESS);
    }
    let count = |action| report.applied.iter().filter(|a| a.action == action).count();
    let mut out = stdout();
    writeln!(
        out,
        "installed {}, removed {}, created {} directories, removed {} directories",
        count(Action::Install),
        count(Action::Remove),
        count(Action::CreateDirectory),
        count(Action::RemoveDirectory),
    )?;
    if !report.index_published {
        writeln!(out, "index not updated")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

pub fn worktree(command: WorktreeCommand, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    match command {
        WorktreeCommand::List => list_worktrees(&repository, context.git)?,
        WorktreeCommand::Lock { worktree, reason } => {
            let registration = registration(&repository, &worktree)?;
            repository
                .lock_worktree(&registration, &reason)
                .wrap_err_with(|| format!("locking {worktree}"))?;
        }
        WorktreeCommand::Unlock { worktree } => {
            let registration = registration(&repository, &worktree)?;
            let reason = std::fs::read_to_string(registration.join("locked"))
                .wrap_err_with(|| format!("{worktree} is not locked"))?;
            repository
                .unlock_worktree(&registration, &reason)
                .wrap_err_with(|| format!("unlocking {worktree}"))?;
        }
        WorktreeCommand::Prune { worktree } => {
            let registration = registration(&repository, &worktree)?;
            repository
                .prune_worktree(
                    &registration,
                    SystemTime::now(),
                    WorktreeRetirement::Confirmed,
                )
                .wrap_err_with(|| format!("pruning {worktree}"))?;
        }
        WorktreeCommand::Repair { worktree, path } => {
            let registration = registration(&repository, &worktree)?;
            repository
                .repair_worktree(&registration, &path)
                .wrap_err_with(|| format!("repairing {worktree}"))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn list_worktrees(repository: &Repository, git: bool) -> Result<()> {
    let common = repository.common_dir();
    let mut rows = Vec::new();
    if let Some(main) = common
        .file_name()
        .is_some_and(|name| name == ".git")
        .then(|| common.parent())
        .flatten()
    {
        rows.push((main.to_owned(), common.to_owned(), None));
    }
    for worktree in repository
        .worktrees(MAX_WORKTREES, &AtomicBool::new(false))
        .wrap_err("listing worktrees")?
    {
        let state = match worktree.state {
            WorktreeState::Available => None,
            WorktreeState::Missing => Some("missing"),
            _ => Some("unreadable"),
        };
        let path = worktree.path.unwrap_or_else(|| worktree.git_dir.clone());
        rows.push((path, worktree.git_dir, state));
    }
    let references = repository.references().wrap_err("opening references")?;
    let width = rows
        .iter()
        .map(|(path, ..)| path.as_os_str().len())
        .max()
        .unwrap_or(0);
    let objects = repository.objects()?;
    let abbrev = crate::common::abbrev_len(repository, &objects)?;
    let mut formatted = Vec::new();
    for (path, git_dir, state) in rows {
        let head = std::fs::read(git_dir.join("HEAD")).unwrap_or_default();
        let head = head.trim_ascii_end();
        let (id, branch) = match head.strip_prefix(b"ref: ") {
            Some(name) => {
                let id = RefName::new(name)
                    .ok()
                    .map(|name| resolve_ref(&references, &name))
                    .transpose()?
                    .flatten();
                (id, Some(name))
            }
            None => (display_bytes(head).parse::<ObjectId>().ok(), None),
        };
        let id = id.unwrap_or_else(|| ObjectId::null(repository.object_format()));
        let id = if git {
            objects.abbreviate(id, abbrev)?.to_string()
        } else {
            id.to_string()
        };
        let branch = match branch {
            Some(name) => {
                let short = name.strip_prefix(b"refs/heads/").unwrap_or(name);
                format!("[{}]", display_bytes(short))
            }
            None => "(detached HEAD)".to_owned(),
        };
        formatted.push((path, id, branch, state));
    }
    let abbrev = formatted
        .iter()
        .map(|(_, id, _, _)| id.len())
        .max()
        .unwrap_or(0)
        .max(abbrev);
    let mut out = stdout();
    for (path, id, branch, state) in formatted {
        if git {
            writeln!(
                out,
                "{:width$} {id:abbrev$} {branch}",
                path.display(),
                width = width
            )?;
        } else {
            let state = state.map(|s| format!(" {s}")).unwrap_or_default();
            writeln!(out, "{} {id} {branch}{state}", path.display())?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Resolves a registration name or checkout path to its directory under `worktrees/`.
fn registration(repository: &Repository, name: &str) -> Result<PathBuf> {
    let direct = repository.common_dir().join("worktrees").join(name);
    if !name.contains('/') && direct.is_dir() {
        return Ok(direct);
    }
    let wanted = std::fs::canonicalize(name).ok();
    for worktree in repository
        .worktrees(MAX_WORKTREES, &AtomicBool::new(false))
        .wrap_err("listing worktrees")?
    {
        if wanted.is_some()
            && worktree.path.as_deref().and_then(|p| p.canonicalize().ok()) == wanted
        {
            return Ok(worktree.git_dir);
        }
    }
    bail!("no registered worktree named {name}")
}

fn worktree_root(repository: &Repository) -> Result<&Path> {
    repository
        .worktree()
        .ok_or_else(|| eyre!("repository has no working tree"))
}

/// The display label and raw pattern text of a matched rule: worktree-relative where possible.
fn describe<'a>(
    rules: &'a WorktreeRules,
    root: &Path,
    source: usize,
    line: usize,
) -> (String, &'a [u8]) {
    let file = rules.source(source);
    let label = file.map(SourceFile::path).map_or_else(String::new, |path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string()
    });
    let pattern = file.and_then(|file| file.line(line)).unwrap_or_default();
    (label, pattern)
}
