//! Pack inspection and repository maintenance: `girt pack ...` and `girt maintenance ...`.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use girt::pack::{IndexRebuildLimits, IndexRebuildOutcome, rebuild_index};
use girt::retention::{
    MaintenanceIsolation, ReflogExpiryPolicy, RepackLimits, RetentionPlan, RetentionPolicy,
};
use girt::{ObjectFormat, ObjectId, Repository};

use crate::Context;
use crate::common::{not_cancelled, objects, stdout};

/// Loose objects and packs younger than this survive pruning and pack retirement.
const PRUNE_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// Reflog records whose endpoints are still reachable expire after this age.
const REFLOG_LIVE_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// Reflog records whose endpoints are both unreachable expire after this age.
const REFLOG_DISCONNECTED_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Repository maintenance commands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Plan retention: count retained and unreachable objects (read-only).
    ///
    /// Retains everything reachable from refs, reflogs, HEAD and the index. Output is one
    /// `key value` pair per line. There is no exact Git equivalent; `--git` is ignored.
    Plan,
    /// Write every retained object into one new pack (additive; deletes nothing).
    ///
    /// Prints `<path> <objects> <bytes>` for the new pack. There is no exact Git equivalent;
    /// `--git` is ignored.
    Repack,
    /// Run destructive maintenance phases: expire reflogs, retire old packs, prune loose objects.
    ///
    /// No other process (Git, girt, jj, editors, ...) may read or write the repository while
    /// this runs; passing `--yes` asserts that. Without `--yes`, prints the plan and the phases
    /// that would run. Loose objects and packs newer than two weeks are always kept; reflog
    /// records expire after 90 days (30 days when unreachable). There is no exact Git
    /// equivalent; `--git` is ignored.
    Run(RunArgs),
}

/// Options for `girt maintenance run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Delete unreachable loose objects older than two weeks.
    #[arg(long)]
    prune_loose: bool,
    /// Expire old reflog records.
    #[arg(long)]
    expire_reflogs: bool,
    /// Replace packs older than two weeks with one pack of the retained objects.
    #[arg(long)]
    retire_packs: bool,
    /// Actually run; asserts that nothing else uses the repository meanwhile.
    #[arg(long)]
    yes: bool,
}

/// Pack file commands.
#[derive(Debug, clap::Subcommand)]
pub enum PackCommand {
    /// List packs: `<path> <objects> <bytes>` per line, sorted by path.
    ///
    /// There is no exact Git equivalent; `--git` is ignored.
    List,
    /// Verify every pack's checksums and objects; prints `ok` or fails with the error.
    ///
    /// Similar to `git verify-pack` over every pack, but output differs; `--git` is ignored.
    Verify,
    /// Rebuild the `.idx` for an existing `.pack` (like `git index-pack <pack>`).
    ///
    /// Prints `<outcome> <pack checksum> <objects>`; with `--git`, prints the pack checksum
    /// only, as `git index-pack` does.
    Reindex {
        /// Path to the `.pack` file.
        pack: PathBuf,
    },
}

/// Runs a `girt maintenance` command.
pub fn run(command: Command, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    match command {
        Command::Plan => {
            let plan = plan(&repository, &RetentionPolicy::trusted())?;
            print_plan(&repository, &plan)?;
        }
        Command::Repack => {
            let published = repository
                .repack_retained(
                    &RetentionPolicy::trusted(),
                    RepackLimits::trusted(),
                    not_cancelled(),
                )
                .context("repacking retained objects")?;
            let written = published.written;
            let path = pack_dir(&repository).join(format!("pack-{}.pack", written.checksum));
            writeln!(
                stdout(),
                "{} {} {}",
                path.display(),
                written.objects,
                written.pack_bytes
            )?;
        }
        Command::Run(args) => return run_phases(&repository, &args),
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs a `girt pack` command.
pub fn pack(command: PackCommand, context: &Context) -> Result<ExitCode> {
    match command {
        PackCommand::List => {
            let repository = context.repository()?;
            let format = repository.object_format();
            let mut out = stdout();
            for pack in pack_paths(&repository)? {
                let ids = index_ids(&pack.with_extension("idx"), format)?;
                let bytes = std::fs::metadata(&pack)
                    .with_context(|| format!("reading {}", pack.display()))?
                    .len();
                writeln!(out, "{} {} {bytes}", pack.display(), ids.len())?;
            }
        }
        PackCommand::Verify => {
            let repository = context.repository()?;
            match objects(&repository)?.verify_packs(not_cancelled()) {
                Ok(()) => writeln!(stdout(), "ok")?,
                Err(error) => {
                    eprintln!("error: {:#}", anyhow::Error::new(error));
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        PackCommand::Reindex { pack } => {
            // A loose pack needs no repository; fall back to SHA-1 when none is found.
            let format = context
                .repository()
                .map_or(ObjectFormat::Sha1, |repository| repository.object_format());
            let rebuilt = rebuild_index(
                &pack,
                format,
                IndexRebuildLimits::default(),
                not_cancelled(),
            )
            .with_context(|| format!("rebuilding index for {}", pack.display()))?;
            let mut out = stdout();
            if context.git {
                writeln!(out, "{}", rebuilt.checksum)?;
            } else {
                let outcome = match rebuilt.outcome {
                    IndexRebuildOutcome::Created => "created",
                    IndexRebuildOutcome::Replaced => "replaced",
                    IndexRebuildOutcome::Unchanged => "unchanged",
                    _ => "rebuilt",
                };
                writeln!(out, "{outcome} {} {}", rebuilt.checksum, rebuilt.objects)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Isolation asserted by the user's `--yes`: girt has no repository-wide lock, so the user
/// promises that no other process touches the repository during the run.
struct UserAsserted;

impl MaintenanceIsolation for UserAsserted {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> io::Result<()> {
        Ok(())
    }
}

/// Plans, then runs the selected destructive phases in Git's order when `--yes` is given.
fn run_phases(repository: &Repository, args: &RunArgs) -> Result<ExitCode> {
    if !(args.prune_loose || args.expire_reflogs || args.retire_packs) {
        bail!("select at least one of --prune-loose, --expire-reflogs, --retire-packs");
    }
    let now = SystemTime::now();
    let mut policy = RetentionPolicy::trusted();
    if args.prune_loose || args.retire_packs {
        policy = policy.with_recent_cutoff(now - PRUNE_AGE);
    }
    if args.expire_reflogs {
        let before = |age: Duration| {
            let cutoff = now - age;
            cutoff
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| i128::from(since.as_secs()))
        };
        policy = policy.with_reflog_expiry(ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(before(REFLOG_LIVE_AGE)),
            disconnected_before: Some(before(REFLOG_DISCONNECTED_AGE)),
        });
    }

    let mut out = stdout();
    if !args.yes {
        let plan = plan(repository, &policy)?;
        print_plan(repository, &plan)?;
        let (loose, _) = unreachable(repository, &plan)?;
        if args.expire_reflogs {
            let records: usize = plan.reflog_expiry_candidates.values().map(Vec::len).sum();
            writeln!(out, "would-expire-reflog-records {records}")?;
        }
        if args.retire_packs {
            let packs = pack_paths(repository)?;
            let retired = packs
                .iter()
                .filter(|pack| !plan.protected_packs.contains(*pack))
                .count();
            writeln!(out, "would-retire-packs {retired}")?;
        }
        if args.prune_loose {
            // Loose objects newer than the cutoff are roots, so they are never in this set.
            writeln!(out, "would-prune-loose {}", loose.len())?;
        }
        writeln!(
            out,
            "dry run: pass --yes to run (nothing else may use the repository)"
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    let cancel = not_cancelled();
    let mut isolation = UserAsserted;
    if args.expire_reflogs {
        let report = repository
            .expire_reflogs(&mut isolation, &policy, cancel)
            .context("expiring reflogs")?;
        writeln!(out, "expired-reflogs {}", report.changed.len())?;
    }
    if args.retire_packs {
        let report = repository
            .retire_old_packs(&mut isolation, &policy, RepackLimits::trusted(), cancel)
            .context("retiring old packs")?;
        if let Some(published) = report.published {
            writeln!(out, "new-pack pack-{}", published.written.checksum)?;
        }
        for path in &report.removed {
            writeln!(out, "removed {}", path.display())?;
        }
    }
    if args.prune_loose {
        let report = repository
            .prune_unreachable_loose(&mut isolation, &policy, cancel)
            .context("pruning loose objects")?;
        writeln!(out, "pruned-loose {}", report.deleted.len())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Plans retention, failing unless the scan was complete.
fn plan(repository: &Repository, policy: &RetentionPolicy) -> Result<RetentionPlan> {
    let plan = repository.plan_retention(policy, not_cancelled());
    if let girt::retention::RetentionOutcome::Incomplete(error) = &plan.outcome {
        bail!("retention scan incomplete: {error}");
    }
    Ok(plan)
}

/// Prints plan counts as `key value` lines.
fn print_plan(repository: &Repository, plan: &RetentionPlan) -> Result<()> {
    let (loose, packed) = unreachable(repository, plan)?;
    let mut out = stdout();
    writeln!(out, "roots {}", plan.roots.len())?;
    writeln!(out, "retained {}", plan.required.len())?;
    writeln!(out, "reachable {}", plan.reachable.len())?;
    writeln!(out, "unreachable-loose {}", loose.len())?;
    writeln!(out, "unreachable-packed {}", packed.len())?;
    writeln!(out, "protected-packs {}", plan.protected_packs.len())?;
    writeln!(out, "alternates {}", plan.alternate_stores.len())?;
    Ok(())
}

/// Unreachable loose and packed object IDs in the primary store.
fn unreachable(
    repository: &Repository,
    plan: &RetentionPlan,
) -> Result<(BTreeSet<ObjectId>, BTreeSet<ObjectId>)> {
    let format = repository.object_format();
    let keep = |id: &ObjectId| !plan.reachable.contains(id) && !plan.required.contains(id);
    let mut loose: BTreeSet<_> = loose_ids(&objects_dir(repository), format)?;
    loose.retain(keep);
    let mut packed = BTreeSet::new();
    for pack in pack_paths(repository)? {
        packed.extend(index_ids(&pack.with_extension("idx"), format)?);
    }
    packed.retain(keep);
    Ok((loose, packed))
}

fn objects_dir(repository: &Repository) -> PathBuf {
    repository.common_dir().join("objects")
}

fn pack_dir(repository: &Repository) -> PathBuf {
    objects_dir(repository).join("pack")
}

/// Indexed `.pack` files in the primary store, sorted by path.
fn pack_paths(repository: &Repository) -> Result<Vec<PathBuf>> {
    let dir = pack_dir(repository);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    let mut packs = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "pack") && path.with_extension("idx").exists()
        {
            packs.push(path);
        }
    }
    packs.sort();
    Ok(packs)
}

/// Loose object IDs found in `objects/xx/` fan-out directories.
fn loose_ids(dir: &Path, format: ObjectFormat) -> Result<BTreeSet<ObjectId>> {
    let mut ids = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(ids);
    };
    for entry in entries {
        let entry = entry?;
        let prefix = entry.file_name();
        let Some(prefix) = prefix.to_str().filter(|name| name.len() == 2) else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path())? {
            let name = file?.file_name();
            let hex = format!("{prefix}{}", name.to_string_lossy());
            if let Ok(id) = hex.parse::<ObjectId>()
                && id.format() == format
            {
                ids.insert(id);
            }
        }
    }
    Ok(ids)
}

/// Object IDs listed by a version 1 or 2 pack index.
fn index_ids(path: &Path, format: ObjectFormat) -> Result<Vec<ObjectId>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let width = format.digest_len();
    let (fanout, names, stride) = if bytes.starts_with(b"\xfftOc\0\0\0\x02") {
        (8, 8 + 1024, width)
    } else {
        (0, 1024, width + 4)
    };
    let corrupt = || anyhow::anyhow!("corrupt pack index {}", path.display());
    let count = bytes
        .get(fanout + 1020..fanout + 1024)
        .map(|word| u32::from_be_bytes([word[0], word[1], word[2], word[3]]) as usize)
        .ok_or_else(corrupt)?;
    // Version 1 entries are `offset, name`; version 2 names are contiguous.
    let skip = if fanout == 0 { 4 } else { 0 };
    (0..count)
        .map(|position| {
            let start = names + position * stride + skip;
            let raw = bytes.get(start..start + width).ok_or_else(corrupt)?;
            ObjectId::from_bytes(format, raw).map_err(|_| corrupt())
        })
        .collect()
}
