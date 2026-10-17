//! Index, status, ignore, checkout and linked-worktree commands.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::checkout::Action;
use girt::ignore::{Case, Ignore, InputKind, Source};
use girt::index::{Entry, Stage};
use girt::refs::RefName;
use girt::status::{Baseline, Change, Untracked};
use girt::{EntryMode, ObjectId, Repository, TreeValue, WorktreeRetirement, WorktreeState};

use crate::Context;
use crate::common::{
    display_bytes, not_cancelled, objects, resolve_ref, resolve_tree, stdout, write_line,
};

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
    /// Also list untracked files, filtered through .gitignore, info/exclude and
    /// core.excludesFile.
    #[arg(short, long)]
    untracked: bool,
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
/// staged or conflicted paths rather than overwriting them. Content is written raw (no filters or
/// EOL conversion). Afterwards `git status` may report staged changes relative to HEAD.
///
/// Derived index caches (`TREE`, `FSMN`, `UNTR`, `EOIE`, `IEOT`) are dropped for Git to rebuild.
/// Split/sparse indexes, resolve-undo data and unknown extensions are refused.
#[derive(Debug, clap::Args)]
pub struct CheckoutArgs {
    /// Revision whose tree to check out.
    revision: String,
    /// Revision whose tree the current index represents; an unborn HEAD or a missing index (as in
    /// a fresh `clone --no-checkout`) means empty.
    #[arg(long, default_value = "HEAD")]
    from: String,
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
        worktree: String,
        /// Reason recorded in the lock.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Remove a registration's lock.
    Unlock {
        /// Registration name (under `worktrees/`) or checkout path.
        worktree: String,
    },
    /// Delete one registration whose checkout no longer exists, including its HEAD and index.
    Prune {
        /// Registration name (under `worktrees/`).
        worktree: String,
    },
    /// Repair the links between a registration and its (moved) checkout.
    Repair {
        /// Registration name (under `worktrees/`).
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
    let policy = if args.untracked {
        Untracked::RawFilesWithoutIgnores
    } else {
        Untracked::Omit
    };
    let report = repository
        .raw_status(Baseline::Head, policy, Default::default(), not_cancelled())
        .wrap_err("computing status")?;

    let mut lines: BTreeMap<Vec<u8>, [u8; 2]> = BTreeMap::new();
    for change in &report.staged {
        let code = match (&change.old, &change.new) {
            (None, _) => b'A',
            (_, None) => b'D',
            (Some(old), Some(new)) => modified(old, new),
        };
        lines.entry(change.path.clone()).or_insert([b' '; 2])[0] = code;
    }
    for change in &report.unstaged {
        let code = match &change.change {
            Change::Modified(new) => modified(&change.index, new),
            _ => b'D',
        };
        lines.entry(change.path.clone()).or_insert([b' '; 2])[1] = code;
    }
    let mut stages: BTreeMap<&[u8], BTreeSet<Stage>> = BTreeMap::new();
    for entry in &report.unmerged {
        stages.entry(&entry.path).or_default().insert(entry.stage);
    }
    for (path, stages) in stages {
        let has = |stage| stages.contains(&stage);
        let code = match (has(Stage::Base), has(Stage::Ours), has(Stage::Theirs)) {
            (true, false, false) => *b"DD",
            (false, true, false) => *b"AU",
            (true, true, false) => *b"UD",
            (false, false, true) => *b"UA",
            (true, false, true) => *b"DU",
            (false, true, true) => *b"AA",
            _ => *b"UU",
        };
        lines.insert(path.to_vec(), code);
    }

    let mut untracked = Vec::new();
    if args.untracked {
        let index = repository
            .read_index(Default::default())
            .wrap_err("reading index")?;
        let entries = index.as_ref().map_or(&[][..], |index| index.entries());
        let mut rules = IgnoreRules::new(&repository)?;
        let mut leaves = Vec::new();
        for path in &report.raw_untracked {
            if !rules
                .check(path, InputKind::File)?
                .is_some_and(|m| m.ignored)
            {
                leaves.push((path.clone(), false));
            }
        }
        for path in &report.boundaries {
            if !rules
                .check(path, InputKind::Directory)?
                .is_some_and(|m| m.ignored)
            {
                leaves.push((path.clone(), true));
            }
        }
        untracked = if context.git {
            collapse(entries, leaves)
        } else {
            leaves.into_iter().map(|(path, _)| path).collect()
        };
        untracked.sort();
        untracked.dedup();
    }

    let mut out = stdout();
    let reset = crate::color::style(context.color, crate::color::RESET);
    for (path, code) in &lines {
        if context.color {
            let staged = crate::color::GREEN;
            let unstaged = crate::color::RED;
            write!(
                out,
                "{staged}{}{reset}{unstaged}{}{reset}",
                char::from(code[0]),
                char::from(code[1])
            )?;
        } else {
            out.write_all(code)?;
        }
        out.write_all(b" ")?;
        write_path(&mut out, path, context.git)?;
    }
    for path in &untracked {
        if context.color {
            write!(out, "{}??{reset} ", crate::color::RED)?;
        } else {
            out.write_all(b"?? ")?;
        }
        write_path(&mut out, path, context.git)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Status letter for a changed entry: `T` when the kind changes, otherwise `M`.
fn modified(old: &TreeValue, new: &TreeValue) -> u8 {
    let kind = |mode| match mode {
        EntryMode::Blob | EntryMode::Executable => 0,
        EntryMode::Symlink => 1,
        EntryMode::Tree => 2,
        _ => 3,
    };
    if kind(old.mode) == kind(new.mode) {
        b'M'
    } else {
        b'T'
    }
}

/// Collapses untracked leaves to their topmost ancestor directory holding no tracked paths, as
/// `git status` does by default. Boundaries (nested repositories) are shown as directories.
fn collapse(entries: &[Entry], leaves: Vec<(Vec<u8>, bool)>) -> Vec<Vec<u8>> {
    let mut tracked_dirs = HashSet::new();
    for entry in entries {
        for (position, _) in entry.path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            tracked_dirs.insert(&entry.path[..position]);
        }
    }
    leaves
        .into_iter()
        .map(|(path, directory)| {
            let collapsed = path
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'/')
                .map(|(position, _)| position)
                .find(|position| !tracked_dirs.contains(&path[..*position]));
            match collapsed {
                Some(position) => path[..=position].to_vec(),
                None if directory => [path.as_slice(), b"/"].concat(),
                None => path,
            }
        })
        .collect()
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
    let mut rules = IgnoreRules::new(&repository)?;
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
            rules.check(key, kind)?
        };
        any |= found.is_some_and(|m| m.ignored || context.git);
        match (found, context.git) {
            (Some(found), true) => {
                let (source, pattern) = rules.describe(found.source, found.line);
                out.write_all(format!("{source}:{}:", found.line).as_bytes())?;
                out.write_all(pattern)?;
                writeln!(out, "\t{path}")?;
            }
            (_, true) => {}
            (Some(found), false) => {
                let (source, pattern) = rules.describe(found.source, found.line);
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
    let objects = objects(&repository)?;
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
    let report = match repository.checkout_tree_with_progress(
        baseline,
        target,
        Default::default(),
        not_cancelled(),
        |done, total| progress.update("Updating files", done, Some(total)),
    ) {
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
        .worktrees(MAX_WORKTREES, not_cancelled())
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
    let objects = crate::common::objects(repository)?;
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
        .worktrees(MAX_WORKTREES, not_cancelled())
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

/// Ignore rules loaded lazily from the sources Git consults.
struct IgnoreRules {
    root: PathBuf,
    ignore: Ignore,
    /// Display name and content for each added source, by insertion index.
    sources: Vec<(String, Vec<u8>)>,
    loaded: HashSet<Vec<u8>>,
}

impl IgnoreRules {
    /// Loads `core.excludesFile` (or the XDG default) and `info/exclude`.
    fn new(repository: &Repository) -> Result<Self> {
        let root = worktree_root(repository)?.to_owned();
        let case = match repository.config().string("core", None, "ignorecase") {
            Some(value) if value.eq_ignore_ascii_case(b"true") => Case::AsciiInsensitive,
            _ => Case::Sensitive,
        };
        let mut rules = Self {
            root: root.clone(),
            ignore: Ignore::new(case, Default::default()),
            sources: Vec::new(),
            loaded: HashSet::new(),
        };
        let global = match repository.config().string("core", None, "excludesfile") {
            Some(path) => Some(expand_home(&display_bytes(path))),
            None => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
                .map(|dir| dir.join("git/ignore")),
        };
        if let Some(global) = global {
            rules.add(Source::Global, &global, global.display().to_string())?;
        }
        let exclude = repository.common_dir().join("info/exclude");
        let label = exclude
            .strip_prefix(&root)
            .unwrap_or(&exclude)
            .display()
            .to_string();
        rules.add(Source::Info, &exclude, label)?;
        Ok(rules)
    }

    /// Adds a source file if it exists.
    fn add(&mut self, source: Source<'_>, file: &Path, label: String) -> Result<()> {
        let content = match std::fs::read(file) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).wrap_err_with(|| format!("reading {label}")),
        };
        let index = self
            .ignore
            .add(source, &content)
            .wrap_err_with(|| format!("loading {label}"))?;
        debug_assert_eq!(index, self.sources.len());
        self.sources.push((label, content));
        Ok(())
    }

    /// Checks a path after loading the `.gitignore` of the root and every ancestor directory.
    fn check(&mut self, path: &[u8], kind: InputKind) -> Result<Option<girt::ignore::Match>> {
        let ends = std::iter::once(0).chain(
            path.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'/')
                .map(|(position, _)| position),
        );
        for end in ends {
            let directory = path[..end].to_vec();
            if self.loaded.insert(directory.clone()) {
                let relative = display_bytes(&directory).into_owned();
                let label = if relative.is_empty() {
                    ".gitignore".to_owned()
                } else {
                    format!("{relative}/.gitignore")
                };
                let file = self.root.join(&label);
                self.add(Source::Directory(&directory), &file, label)?;
            }
        }
        self.ignore
            .check(path, kind)
            .wrap_err_with(|| format!("checking {}", display_bytes(path)))
    }

    /// The source label and raw pattern text at a one-based line.
    fn describe(&self, source: usize, line: usize) -> (&str, &[u8]) {
        let (label, content) = &self.sources[source];
        let pattern = content
            .split(|b| *b == b'\n')
            .nth(line - 1)
            .unwrap_or_default();
        let pattern = pattern.strip_suffix(b"\r").unwrap_or(pattern);
        (label, pattern)
    }
}

/// Expands a leading `~/` to `$HOME`.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Writes a path line, C-quoting it like Git's `core.quotePath` when `quote` is set.
fn write_path(out: &mut impl Write, path: &[u8], quote: bool) -> std::io::Result<()> {
    let needs = quote
        && path
            .iter()
            .any(|&b| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\');
    if !needs {
        return write_line(out, path);
    }
    out.write_all(b"\"")?;
    for &byte in path {
        match byte {
            b'"' => out.write_all(b"\\\"")?,
            b'\\' => out.write_all(b"\\\\")?,
            b'\t' => out.write_all(b"\\t")?,
            b'\n' => out.write_all(b"\\n")?,
            b'\r' => out.write_all(b"\\r")?,
            0x07 => out.write_all(b"\\a")?,
            0x08 => out.write_all(b"\\b")?,
            0x0b => out.write_all(b"\\v")?,
            0x0c => out.write_all(b"\\f")?,
            0x20..0x7f => out.write_all(&[byte])?,
            _ => write!(out, "\\{byte:03o}")?,
        }
    }
    out.write_all(b"\"\n")
}
