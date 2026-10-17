//! `girt ref`: list, inspect, update, delete and log references.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
use girt::refs::{Dereference, Expected, RefEdit, RefName, References, Reflog, Target};
use girt::{Config, ObjectId, ObjectKind, Repository, Signature};

use crate::Context;
use crate::common::{self, display_bytes};

/// Maximum symbolic-ref hops shown by `girt ref show`.
const MAX_DEPTH: usize = 5;

/// Reference commands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List references (like `git for-each-ref`), sorted by name.
    ///
    /// Prints `<id> <name>` per line; symbolic references append ` -> <target>`. With `--git`,
    /// prints `git for-each-ref`'s default `<id> <type>\t<name>`.
    List {
        /// Only list references equal to or under these prefixes (e.g. `refs/heads`).
        prefixes: Vec<String>,
        /// Only list symbolic references.
        #[arg(long)]
        symbolic: bool,
    },
    /// Show how a reference resolves (like `git rev-parse <name>`).
    ///
    /// Short names are expanded the way Git does. Prints each hop of the symbolic chain and the
    /// final ID, one per line; with `--git`, prints only the ID.
    Show {
        /// Reference name, e.g. `HEAD`, `main` or `refs/tags/v1`.
        name: String,
    },
    /// Point a reference at an object (like `git update-ref`).
    ///
    /// Symbolic references such as `HEAD` are followed and their target is updated. A reflog
    /// entry is appended using `user.name`/`user.email` (or `GIT_COMMITTER_NAME`/`_EMAIL`), with
    /// the current time in UTC (`+0000`): girt does not read the local time zone.
    Set {
        /// Full reference name, e.g. `refs/heads/main`.
        name: String,
        /// Object ID or revision to store.
        value: String,
        /// Only update if the reference currently has this ID.
        #[arg(long, value_name = "OLD_ID", conflicts_with = "create_only")]
        expect: Option<String>,
        /// Only create the reference; fail if it exists.
        #[arg(long)]
        create_only: bool,
        /// Reflog message.
        #[arg(short, long, default_value = "")]
        message: String,
    },
    /// Delete a reference and its reflog (like `git update-ref -d`).
    Delete {
        /// Full reference name.
        name: String,
        /// Only delete if the reference currently has this ID.
        #[arg(long, value_name = "OLD_ID")]
        expect: Option<String>,
    },
    /// Show a reference's reflog, newest first (like `git reflog show`).
    ///
    /// Prints `<new-id> <old-id> <message>` per line. With `--git`, prints
    /// `<abbrev> <name>@{N}: <message>`; IDs honor `core.abbrev`, automatic repository-size
    /// scaling, and uniqueness across all stored object kinds.
    Log {
        /// Reference name, e.g. `HEAD` or `main`.
        name: String,
    },
}

pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    let references = repository.references().wrap_err("opening references")?;
    match command {
        Command::List { prefixes, symbolic } => {
            list(&repository, &references, &prefixes, symbolic, context)
        }
        Command::Show { name } => show(&references, &name, context),
        Command::Set {
            name,
            value,
            expect,
            create_only,
            message,
        } => {
            let id = common::resolve(&repository, &value)?;
            check_target(&repository, &references, &name, id)?;
            let expected = match (expect, create_only) {
                (Some(old), _) => Expected::Value(Target::Direct(parse_id(&old)?)),
                (None, true) => Expected::Absent,
                (None, false) => Expected::Any,
            };
            let reflog = Reflog::Append {
                committer: committer(&repository)?,
                message: message.into_bytes(),
            };
            edit(
                &references,
                &name,
                Some(Target::Direct(id)),
                expected,
                reflog,
            )
        }
        Command::Delete { name, expect } => {
            let expected = match expect {
                Some(old) => Expected::Value(Target::Direct(parse_id(&old)?)),
                None => Expected::Exists,
            };
            edit(&references, &name, None, expected, Reflog::Delete)
        }
        Command::Log { name } => log(&repository, &references, &name, context),
    }
}

fn parse_id(hex: &str) -> Result<ObjectId> {
    hex.parse()
        .map_err(|_| eyre!("`{hex}` is not a full object ID"))
}

/// Refuses missing objects, and non-commits for branches (following a symbolic `name`), as Git
/// does.
fn check_target(
    repository: &Repository,
    references: &References<'_>,
    name: &str,
    id: ObjectId,
) -> Result<()> {
    let objects = common::objects(repository)?;
    let object = objects
        .read(id, common::read_limits())
        .wrap_err_with(|| format!("reading {id}"))?
        .ok_or_else(|| eyre!("object {id} does not exist"))?;
    let destination = references.resolve(&ref_name(name)?, 5).map_or_else(
        |_| name.as_bytes().to_vec(),
        |resolution| resolution.name.as_bytes().to_vec(),
    );
    if destination.starts_with(b"refs/heads/") && object.kind() != ObjectKind::Commit {
        bail!(
            "{} must point at a commit, not a {:?}",
            display_bytes(&destination),
            object.kind()
        );
    }
    Ok(())
}

fn ref_name(name: &str) -> Result<RefName> {
    RefName::new(name).map_err(|_| eyre!("invalid reference name `{name}`"))
}

/// Finds the first existing full name for a possibly short name.
fn find(references: &References<'_>, name: &str) -> Result<(RefName, Target)> {
    for candidate in common::candidates(name) {
        let Ok(full) = RefName::new(&candidate) else {
            continue;
        };
        if let Some(target) = references
            .read(&full)
            .wrap_err_with(|| format!("reading {candidate}"))?
        {
            return Ok((full, target));
        }
    }
    bail!("unknown reference `{name}`")
}

fn list(
    repository: &Repository,
    references: &References<'_>,
    prefixes: &[String],
    symbolic: bool,
    context: &Context,
) -> Result<ExitCode> {
    let objects = common::objects(repository)?;
    let mut all = references.list().wrap_err("listing references")?;
    all.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let mut out = common::stdout();
    for reference in all {
        let name = reference.name.as_bytes();
        let matches = prefixes.is_empty()
            || prefixes.iter().any(|prefix| {
                let prefix = prefix.trim_end_matches('/').as_bytes();
                name == prefix
                    || (name.starts_with(prefix) && name.get(prefix.len()) == Some(&b'/'))
            });
        let is_symbolic = matches!(reference.target, Target::Symbolic(_));
        if !matches || (symbolic && !is_symbolic) {
            continue;
        }
        let id = match &reference.target {
            Target::Direct(id) => *id,
            // Dangling symbolic refs are skipped, as Git does.
            _ => match common::resolve_ref(references, &reference.name)? {
                Some(id) => id,
                None => continue,
            },
        };
        let name = display_bytes(name);
        if context.git {
            let object = objects
                .read(id, common::read_limits())
                .wrap_err_with(|| format!("reading {id}"))?
                .ok_or_else(|| eyre!("missing object {id} for {name}"))?;
            writeln!(out, "{id} {}\t{name}", object.kind().as_str())?;
        } else if let Target::Symbolic(target) = &reference.target {
            writeln!(out, "{id} {name} -> {}", display_bytes(target.as_bytes()))?;
        } else {
            writeln!(out, "{id} {name}")?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn show(references: &References<'_>, name: &str, context: &Context) -> Result<ExitCode> {
    let (mut current, mut target) = find(references, name)?;
    let mut out = common::stdout();
    for _ in 0..=MAX_DEPTH {
        match target {
            Target::Direct(id) => {
                if !context.git {
                    write!(out, "{} -> ", display_bytes(current.as_bytes()))?;
                }
                writeln!(out, "{id}")?;
                out.flush()?;
                return Ok(ExitCode::SUCCESS);
            }
            Target::Symbolic(next) => {
                if !context.git {
                    writeln!(
                        out,
                        "{} -> {}",
                        display_bytes(current.as_bytes()),
                        display_bytes(next.as_bytes())
                    )?;
                }
                target = references
                    .read(&next)
                    .wrap_err_with(|| format!("reading {}", display_bytes(next.as_bytes())))?
                    .ok_or_else(|| eyre!("{} does not exist", display_bytes(next.as_bytes())))?;
                current = next;
            }
            _ => bail!("girt does not support this reference target yet"),
        }
    }
    bail!("symbolic reference chain for `{name}` is too deep")
}

fn edit(
    references: &References<'_>,
    name: &str,
    target: Option<Target>,
    expected: Expected,
    reflog: Reflog,
) -> Result<ExitCode> {
    let edit = RefEdit::new(ref_name(name)?, target, expected, reflog)
        .with_dereference(Dereference::Follow);
    references
        .transaction(&[edit])
        .wrap_err_with(|| format!("updating {name}"))?;
    Ok(ExitCode::SUCCESS)
}

fn log(
    repository: &Repository,
    references: &References<'_>,
    name: &str,
    context: &Context,
) -> Result<ExitCode> {
    let (full, _) = find(references, name)?;
    let entries = references
        .reflog(&full)
        .wrap_err_with(|| format!("reading reflog of {name}"))?
        .unwrap_or_default();
    let objects = common::objects(repository)?;
    let width = common::abbrev_len(repository, &objects)?;
    let mut out = common::stdout();
    for (index, entry) in entries.iter().rev().enumerate() {
        let message = display_bytes(&entry.message);
        if context.git {
            let hex = objects.abbreviate(entry.new, width)?.to_string();
            writeln!(out, "{} {name}@{{{index}}}: {message}", hex)?;
        } else {
            writeln!(out, "{} {} {message}", entry.new, entry.old)?;
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Builds the reflog committer from the environment or `user.name`/`user.email` config.
fn committer(repository: &Repository) -> Result<Signature> {
    let global = global_config();
    let lookup = |env: &str, key: &str| -> Option<Vec<u8>> {
        std::env::var(env).ok().map(String::into_bytes).or_else(|| {
            [Some(repository.config()), global.as_ref()]
                .into_iter()
                .flatten()
                .find_map(|config| config.string("user", None, key).map(<[u8]>::to_vec))
        })
    };
    let (Some(name), Some(email)) = (
        lookup("GIT_COMMITTER_NAME", "name"),
        lookup("GIT_COMMITTER_EMAIL", "email"),
    ) else {
        bail!("no committer identity for the reflog: set user.name and user.email in Git config");
    };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before 1970")?
        .as_secs();
    Ok(Signature {
        name,
        email,
        seconds: i64::try_from(seconds)?,
        offset_minutes: 0,
    })
}

/// Reads the user's global Git config (`$XDG_CONFIG_HOME/git/config`, then `~/.gitconfig`).
fn global_config() -> Option<Config> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let files = xdg
        .map(|xdg| xdg.join("git/config"))
        .into_iter()
        .chain(home.map(|home| home.join(".gitconfig")))
        .filter(|path| path.exists())
        .map(|path| ConfigFile::new(path, ConfigScope::Global))
        .collect();
    Config::resolve(&ConfigInputs::default().with_files(files)).ok()
}
