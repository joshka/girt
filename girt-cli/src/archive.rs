//! `girt archive`: write a tree as a tar or zip archive, as `git archive` does.
use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::archive::{ExtraFile, Format, Options};
use girt::pathspec::{self, Pathspec, Prune};
use girt::{EntryMode, ObjectId, ObjectKind, Objects, Repository};

use crate::{Context, common};

/// Write a commit or tree as a tar, tar.gz or zip archive (`git archive`).
///
/// Takes `git archive`'s options and writes its bytes: tar is byte-identical, and tar.gz and zip
/// match Git's layout. Supports `--format=tar|tgz|tar.gz|zip` (or a format chosen from
/// `-o`'s extension), `-o`, `--prefix`, `--add-file`, `--add-virtual-file`, `--mtime=@<seconds>`,
/// `--worktree-attributes`, `-0` to `-9`, `-l` and pathspecs. `export-ignore`, `export-subst` and
/// content filters apply, and smudge commands run as in Git. `--remote` and `--exec` are refused.
#[derive(Debug, clap::Args)]
#[command(disable_help_flag = true)]
pub struct Args {
    /// `git archive` options, the tree-ish and pathspecs.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    args: Vec<OsString>,
    #[arg(skip)]
    raw: Option<Vec<OsString>>,
}

impl Args {
    /// Replaces the parsed arguments with those
    /// [`common::split_raw_args`](crate::common::split_raw_args) took before clap ran.
    pub fn set_raw(&mut self, raw: Vec<OsString>) {
        self.raw = Some(raw);
    }
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    let raw = args.raw.unwrap_or(args.args);
    let mut format = None;
    let mut output: Option<OsString> = None;
    let mut prefix = Vec::new();
    let mut options = Options::new();
    let mut positional = Vec::new();
    let mut args = raw.into_iter();
    while let Some(arg) = args.next() {
        let Some(text) = arg.to_str().map(str::to_owned) else {
            positional.push(arg);
            continue;
        };
        if text == "--" {
            positional.extend(args.by_ref());
            break;
        }
        let (name, inline) = match text.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_owned(), Some(value.to_owned()))
            }
            _ => (text.clone(), None),
        };
        let mut value = |option: &str| -> Result<String> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| eyre!("option `{option}` requires a value")),
            }
        };
        match name.as_str() {
            "--format" => format = Some(parse_format(&value("--format")?)?),
            "-o" | "--output" => output = Some(OsString::from(value("--output")?)),
            "--prefix" => prefix = value("--prefix")?.into_bytes(),
            "--add-file" => {
                let file = value("--add-file")?;
                options = options.with_extra_file(read_extra(&file, &prefix)?);
            }
            "--add-virtual-file" => {
                let spec = value("--add-virtual-file")?;
                let (path, content) = spec
                    .split_once(':')
                    .ok_or_else(|| eyre!("--add-virtual-file expects <path>:<content>"))?;
                options = options.with_extra_file(ExtraFile::new(path, content));
            }
            "--mtime" => options = options.with_mtime(Some(parse_mtime(&value("--mtime")?)?)),
            "--worktree-attributes" => options = options.with_worktree_attributes(true),
            // Git lists archived paths on stderr; girt writes the archive without the list.
            "-v" | "--verbose" => {}
            "-l" | "--list" => {
                let mut out = common::stdout();
                for name in ["tar", "tgz", "tar.gz", "zip"] {
                    writeln!(out, "{name}")?;
                }
                out.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
            "--remote" | "--exec" => bail!("{name} is unsupported"),
            _ if name.len() == 2
                && name.starts_with('-')
                && name.as_bytes().get(1).is_some_and(u8::is_ascii_digit) =>
            {
                let level = name
                    .as_bytes()
                    .get(1)
                    .map_or(6, |digit| u32::from(digit - b'0'));
                options = options.with_level(Some(level));
            }
            _ if name.starts_with('-') && name != "-" => bail!("unknown option `{name}`"),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let revision = positional
        .next()
        .ok_or_else(|| eyre!("a tree-ish is required"))?;
    let revision = revision
        .to_str()
        .ok_or_else(|| eyre!("tree-ish is not UTF-8"))?
        .to_owned();
    let specs: Vec<OsString> = positional.collect();
    let format = match (format, &output) {
        (Some(format), _) => format,
        (None, Some(output)) => format_from_name(output),
        (None, None) => Format::Tar,
    };
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let directory = current_prefix(&repository, context);
    let id = common::resolve(&repository, &revision)?;
    let pathspec = Pathspec::parse(
        specs.iter().map(|spec| spec.as_encoded_bytes()),
        &pathspec::Context::from_environment(&context.environment)?,
    )?;
    for spec in &specs {
        let single = Pathspec::parse([spec.as_encoded_bytes()], &pathspec::Context::new())?;
        let tree = subtree(&objects, tree_of(&objects, id)?, &directory)?;
        if !tree_matches(&objects, tree, &mut Vec::new(), &single)? {
            bail!(
                "pathspec '{}' did not match any files",
                Path::new(spec).display()
            );
        }
    }
    let options = options
        .with_format(format)
        .with_prefix(prefix)
        .with_subdirectory(directory)
        .with_commands(girt::filter::Commands::Run)
        .with_environment(context.environment.clone());
    match output {
        Some(output) => {
            let file = std::fs::File::create(&output)
                .wrap_err_with(|| format!("creating {}", Path::new(&output).display()))?;
            let mut file = std::io::BufWriter::new(file);
            repository.archive(id, &pathspec, &options, &mut file)?;
            file.flush()?;
        }
        None => {
            let mut out = common::stdout();
            repository.archive(id, &pathspec, &options, &mut out)?;
            out.flush()?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_format(name: &str) -> Result<Format> {
    Ok(match name {
        "tar" => Format::Tar,
        "tgz" | "tar.gz" => Format::TarGz,
        "zip" => Format::Zip,
        _ => bail!("unknown archive format '{name}'"),
    })
}

/// The format `git archive -o` picks from a file name, tar when none matches.
fn format_from_name(name: &OsString) -> Format {
    let name = name.to_string_lossy();
    if name.ends_with(".zip") {
        Format::Zip
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        Format::TarGz
    } else {
        Format::Tar
    }
}

/// `--mtime` as `@<seconds>`; Git's other date forms are refused.
fn parse_mtime(value: &str) -> Result<i64> {
    value
        .strip_prefix('@')
        .and_then(|seconds| seconds.parse().ok())
        .ok_or_else(|| eyre!("unsupported --mtime '{value}'; use @<seconds>"))
}

/// An `--add-file`: named by its base name after the current `prefix`, executable when any
/// execute bit is set.
fn read_extra(file: &str, prefix: &[u8]) -> Result<ExtraFile> {
    let path = Path::new(file);
    let content = std::fs::read(path).wrap_err_with(|| format!("reading {file}"))?;
    let name = path
        .file_name()
        .ok_or_else(|| eyre!("--add-file needs a file name: {file}"))?;
    let mut archived = prefix.to_vec();
    archived.extend_from_slice(name.as_encoded_bytes());
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)?.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Ok(ExtraFile::new(archived, content).with_executable(executable))
}

/// The current directory relative to the working tree root, without a trailing `/`.
fn current_prefix(repository: &Repository, context: &Context) -> String {
    if let Some(worktree) = repository.worktree()
        && let Ok(worktree) = worktree.canonicalize()
        && let Ok(current) = context.directory().canonicalize()
        && let Ok(prefix) = current.strip_prefix(&worktree)
    {
        return prefix
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
    }
    String::new()
}

/// The tree `id` names, peeling commits and tags.
fn tree_of(objects: &Objects, mut id: ObjectId) -> Result<ObjectId> {
    loop {
        let header = objects
            .header(id, girt::ReadLimits::default())?
            .ok_or_else(|| eyre!("object {id} is missing"))?;
        match header.kind() {
            ObjectKind::Tree => return Ok(id),
            ObjectKind::Commit => return Ok(objects.read_commit(id)?.tree()),
            ObjectKind::Tag => id = objects.read_tag(id)?.target(),
            ObjectKind::Blob => bail!("{id} is not a tree-ish"),
        }
    }
}

/// The tree at `path` (empty for `tree` itself) below `tree`.
fn subtree(objects: &Objects, mut tree: ObjectId, path: &str) -> Result<ObjectId> {
    for name in path.split('/').filter(|name| !name.is_empty()) {
        tree = objects
            .read_tree(tree)?
            .entries()
            .iter()
            .find(|entry| entry.name == name.as_bytes() && entry.mode == EntryMode::Tree)
            .map(|entry| entry.id)
            .ok_or_else(|| eyre!("the current directory is not in the archived tree"))?;
    }
    Ok(tree)
}

/// Whether any path below `tree` matches `pathspec`, as Git checks each spec before archiving.
fn tree_matches(
    objects: &Objects,
    tree: ObjectId,
    directory: &mut Vec<u8>,
    pathspec: &Pathspec,
) -> Result<bool> {
    for entry in objects.read_tree(tree)?.entries() {
        let length = directory.len();
        if !directory.is_empty() {
            directory.push(b'/');
        }
        directory.extend_from_slice(&entry.name);
        let found = if entry.mode == EntryMode::Tree {
            match pathspec.directory(directory) {
                Prune::Skip => false,
                Prune::All => true,
                Prune::Descend => tree_matches(objects, entry.id, directory, pathspec)?,
            }
        } else {
            pathspec.matches(directory).unwrap_or(true)
        };
        directory.truncate(length);
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}
