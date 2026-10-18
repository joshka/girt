//! Shared state and argument helpers for every command.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::config::ConfigInputs;
use girt::pathspec::{self, Pathspec};
use girt::refs::{RefName, References};
use girt::revision::{Range, Resolver, Revision, Selection};
use girt::workflow::Environment;
use girt::{Config, ObjectId, ObjectKind, Objects, Repository};

/// Global options shared by every command.
#[derive(Debug)]
pub struct Context {
    directory: PathBuf,
    /// Print the equivalent Git command's exact output.
    pub git: bool,
    /// Color human output on stdout.
    pub color: bool,
    /// Allow progress when stderr is a terminal.
    pub progress: bool,
    /// The process environment, captured once; every configuration lookup reads this.
    pub environment: Environment,
}

impl Context {
    /// `-C` is applied by changing the process directory, so every relative path, including
    /// file arguments, resolves the way it does under `git -C`.
    pub fn new(git: bool) -> Self {
        Self {
            directory: PathBuf::from("."),
            git,
            color: false,
            progress: !git,
            environment: Environment::from_process(),
        }
    }

    /// The directory the command runs in.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Git's system, global and environment configuration sources for this environment.
    pub fn config_inputs(&self) -> Result<ConfigInputs> {
        ConfigInputs::git_default(&self.environment).wrap_err("reading config environment")
    }

    /// Discovers the repository containing the working directory, with Git's configuration.
    ///
    /// Every command opens its repository here, so system and global settings such as
    /// `core.excludesFile` and `user.name` apply everywhere.
    pub fn repository(&self) -> Result<Repository> {
        let no_repository = || format!("no repository at {}", self.directory.display());
        let relative_selector = ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"]
            .iter()
            .any(|name| {
                self.environment
                    .get(name)
                    .is_some_and(|path| !path.is_empty() && Path::new(path).is_relative())
            });
        if !relative_selector {
            return Repository::discover_with_config(&self.directory, &self.config_inputs()?)
                .wrap_err_with(no_repository);
        }
        // Inside a working tree, Git resolves a relative selector from the top of the tree.
        let discovered = Repository::discover(&self.directory).wrap_err_with(no_repository)?;
        let Some(top) = discovered.worktree() else {
            return Repository::open_with_config(discovered.git_dir(), &self.config_inputs()?)
                .wrap_err_with(no_repository);
        };
        let environment = self.environment.clone().with_current_dir(top);
        let inputs =
            ConfigInputs::git_default(&environment).wrap_err("reading config environment")?;
        Repository::open_with_config(top, &inputs).wrap_err_with(no_repository)
    }

    /// System, global and environment configuration, for commands that run outside a repository.
    pub fn user_config(&self) -> Result<Config> {
        Config::resolve(&self.config_inputs()?).wrap_err("reading global config")
    }

    /// Compiles command-line pathspecs relative to the current directory inside the worktree.
    ///
    /// The `GIT_*_PATHSPECS` variables apply. Outside the worktree (or in a bare repository) specs
    /// resolve from the repository root. No specs select every path, and a list of only excludes
    /// starts from the whole repository, as `git status`, `git diff` and `git log` do.
    pub fn pathspec(&self, repository: &Repository, specs: &[OsString]) -> Result<Pathspec> {
        let mut context =
            pathspec::Context::from_environment(&self.environment)?.with_excludes_from_root(true);
        if let Some(worktree) = repository.worktree()
            && let Ok(worktree) = worktree.canonicalize()
            && let Ok(current) = self.directory.canonicalize()
            && let Ok(prefix) = current.strip_prefix(&worktree)
        {
            context = context.with_prefix(prefix.as_os_str().as_encoded_bytes());
        }
        let specs = specs.iter().map(|spec| spec.as_encoded_bytes());
        Ok(Pathspec::parse(specs, &context)?)
    }
}

/// Resolves a revision expression such as `main~2`, `v1.0^{tree}` or `HEAD:path`; see
/// [`girt::revision`] for the syntax.
pub fn resolve(repository: &Repository, revision: &str) -> Result<ObjectId> {
    let objects = repository.objects()?;
    resolve_with(repository, &objects, revision, None)
}

/// Resolves a full ref name through symbolic refs; `None` when absent or unborn.
pub fn resolve_ref(references: &References<'_>, name: &RefName) -> Result<Option<ObjectId>> {
    let resolution = references
        .resolve(name)
        .wrap_err_with(|| format!("resolving {}", display_bytes(name.as_bytes())))?;
    Ok(resolution.id)
}

/// Resolves a revision and peels it to an object of `kind`; commits peel to their tree when
/// `kind` is [`ObjectKind::Tree`].
pub fn resolve_kind(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
    kind: ObjectKind,
) -> Result<ObjectId> {
    resolve_with(repository, objects, revision, Some(kind))
}

fn resolve_with(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
    kind: Option<ObjectKind>,
) -> Result<ObjectId> {
    let parsed: Revision = revision.parse()?;
    let resolver = Resolver::new(repository, objects)?;
    let id = match kind {
        Some(kind) => resolver.resolve_to(&parsed, kind),
        None => resolver.resolve(&parsed),
    };
    id.wrap_err_with(|| format!("resolving `{revision}`"))
}

/// Resolves revision and range arguments (`A..B`, `A...B`, `^A`) to the commits to walk;
/// no arguments means `HEAD`.
pub fn select(repository: &Repository, objects: &Objects, ranges: &[String]) -> Result<Selection> {
    let parsed = if ranges.is_empty() {
        vec![Range::Include(Revision::head())]
    } else {
        ranges
            .iter()
            .map(|range| range.parse())
            .collect::<Result<Vec<Range>, _>>()?
    };
    let resolver = Resolver::new(repository, objects)?;
    Ok(resolver.select(&parsed)?)
}

/// Resolves a revision to a commit.
pub fn resolve_commit(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
) -> Result<ObjectId> {
    resolve_kind(repository, objects, revision, ObjectKind::Commit)
}

/// Resolves a revision to a tree, peeling commits.
pub fn resolve_tree(
    repository: &Repository,
    objects: &Objects,
    revision: &str,
) -> Result<ObjectId> {
    resolve_kind(repository, objects, revision, ObjectKind::Tree)
}

/// Formats bytes for display, replacing invalid UTF-8.
pub fn display_bytes(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Locked, buffered standard output. Broken pipes end the command quietly in `main`.
pub fn stdout() -> io::BufWriter<io::StdoutLock<'static>> {
    io::BufWriter::new(io::stdout().lock())
}

/// Writes raw bytes followed by a newline.
pub fn write_line(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(bytes)?;
    out.write_all(b"\n")
}

/// Writes a path line, C-quoting it like Git's default `core.quotePath` when `quote` is set.
pub fn write_path(out: &mut impl Write, path: &[u8], quote: bool) -> io::Result<()> {
    if quote {
        writeln!(out, "{}", girt::display::quote_path(path))
    } else {
        write_line(out, path)
    }
}

/// Resolves `core.abbrev` once for an output operation; collisions are lengthened per ID.
pub fn abbrev_len(repository: &Repository, objects: &Objects) -> Result<usize> {
    let Some(value) = repository.config().value("core", None, "abbrev") else {
        return Ok(objects.auto_abbrev_len());
    };
    let value = value.ok_or_else(|| eyre!("core.abbrev requires a value"))?;
    if value.eq_ignore_ascii_case(b"auto") {
        return Ok(objects.auto_abbrev_len());
    }
    if [b"false".as_slice(), b"no", b"off", b""]
        .iter()
        .any(|v| value.eq_ignore_ascii_case(v))
    {
        // Git uses its maximum supported hash width for disabled abbreviation; worktree
        // columns retain this width even in SHA-1 repositories.
        return Ok(64);
    }
    let length = repository
        .config()
        .integer("core", None, "abbrev")?
        .unwrap_or(0);
    if length < 4 {
        bail!("invalid core.abbrev: {}", display_bytes(value));
    }
    Ok(usize::try_from(i32::try_from(length)?)?)
}

/// Loads presentation identities, expanding only the current user's home in the CLI.
pub fn mailmap(
    repository: &girt::Repository,
    objects: &girt::Objects,
) -> Result<girt::mailmap::Mailmap> {
    if let Some(file) = repository.config().string("mailmap", None, "file")
        && let Some(suffix) = file.strip_prefix(b"~/")
    {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or_else(|| color_eyre::eyre::eyre!("mailmap.file requires a home directory"))?;
        #[cfg(unix)]
        let suffix = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(suffix)
        };
        #[cfg(not(unix))]
        let suffix = std::ffi::OsStr::new(std::str::from_utf8(suffix)?);
        let path = std::path::PathBuf::from(home).join(suffix);
        return Ok(girt::mailmap::Mailmap::load_with_file(
            repository, objects, &path,
        )?);
    }
    Ok(girt::mailmap::Mailmap::load(repository, objects)?)
}
