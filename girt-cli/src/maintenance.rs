//! Pack inspection and repository maintenance: `girt pack ...` and `girt maintenance ...`.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::pack::{IndexRebuildLimits, IndexRebuildOutcome, rebuild_index};
use girt::retention::{
    MaintenanceIsolation, ReflogExpiryPolicy, RepackLimits, RetentionPlan, RetentionPolicy,
};
use girt::{ObjectFormat, ObjectId, ObjectStore, Objects, Repository};

use crate::Context;
use crate::common::stdout;
use crate::object::loose_ids;

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
    /// List packs: `<path> <objects> <bytes>` per line, primary store first, then alternates,
    /// each sorted by path.
    ///
    /// `keep`, `promisor` and `alternate` follow when they apply. There is no exact Git
    /// equivalent; `--git` is ignored.
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
            let progress =
                crate::progress::Activity::new(context.progress, "Repacking retained objects");
            let published = repository
                .repack_retained(
                    &RetentionPolicy::trusted(),
                    RepackLimits::trusted(),
                    &AtomicBool::new(false),
                )
                .wrap_err("repacking retained objects")?;
            let written = published.written;
            progress.finish(format_args!(
                "{} objects, {} bytes",
                written.objects, written.pack_bytes
            ));
            let path = repository
                .object_dir()
                .join("pack")
                .join(format!("pack-{}.pack", written.checksum));
            writeln!(
                stdout(),
                "{} {} {}",
                path.display(),
                written.objects,
                written.pack_bytes
            )?;
        }
        Command::Run(args) => return run_phases(&repository, &args, context.progress),
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs a `girt pack` command.
pub fn pack(command: PackCommand, context: &Context) -> Result<ExitCode> {
    match command {
        PackCommand::List => {
            let repository = context.repository()?;
            let objects = repository.objects()?;
            let mut out = stdout();
            for store in objects.stores() {
                for pack in store.packs() {
                    write!(
                        out,
                        "{} {} {}",
                        pack.path().display(),
                        pack.object_count(),
                        pack.pack_bytes()
                    )?;
                    let marks = [
                        (pack.is_kept(), "keep"),
                        (pack.is_promisor(), "promisor"),
                        (store.is_alternate(), "alternate"),
                    ];
                    for (_, mark) in marks.iter().filter(|(set, _)| *set) {
                        write!(out, " {mark}")?;
                    }
                    writeln!(out)?;
                }
            }
        }
        PackCommand::Verify => {
            let repository = context.repository()?;
            let progress = crate::progress::Activity::new(context.progress, "Verifying packs");
            match repository.objects()?.verify_packs(&AtomicBool::new(false)) {
                Ok(()) => {
                    progress.finish("verified");
                    writeln!(stdout(), "ok")?;
                }
                Err(error) => {
                    progress.clear();
                    eprintln!("error: {:#}", color_eyre::eyre::Report::new(error));
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        PackCommand::Reindex { pack } => {
            // A loose pack needs no repository; fall back to SHA-1 when none is found.
            let format = context
                .repository()
                .map_or(ObjectFormat::Sha1, |repository| repository.object_format());
            let progress =
                crate::progress::Activity::new(context.progress, "Rebuilding pack index");
            let rebuilt = rebuild_index(
                &pack,
                format,
                IndexRebuildLimits::default(),
                &AtomicBool::new(false),
            )
            .wrap_err_with(|| format!("rebuilding index for {}", pack.display()))?;
            progress.finish(format_args!("{} objects", rebuilt.objects));
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
fn run_phases(repository: &Repository, args: &RunArgs, progress_enabled: bool) -> Result<ExitCode> {
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
            let objects = repository.objects()?;
            let retired = primary(&objects)?
                .packs()
                .filter(|pack| !plan.protected_packs.contains(pack.path()))
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

    let cancel = &AtomicBool::new(false);
    let mut isolation = UserAsserted;
    if args.expire_reflogs {
        let progress = crate::progress::Activity::new(progress_enabled, "Expiring reflogs");
        let report = repository
            .expire_reflogs(&mut isolation, &policy, cancel)
            .wrap_err("expiring reflogs")?;
        progress.finish(format_args!("{} reflogs", report.changed.len()));
        writeln!(out, "expired-reflogs {}", report.changed.len())?;
    }
    if args.retire_packs {
        let progress = crate::progress::Activity::new(progress_enabled, "Retiring packs");
        let report = repository
            .retire_old_packs(&mut isolation, &policy, RepackLimits::trusted(), cancel)
            .wrap_err("retiring old packs")?;
        progress.finish(format_args!("{} packs removed", report.removed.len()));
        if let Some(published) = report.published {
            writeln!(out, "new-pack pack-{}", published.written.checksum)?;
        }
        for path in &report.removed {
            writeln!(out, "removed {}", path.display())?;
        }
    }
    if args.prune_loose {
        let progress = crate::progress::Activity::new(progress_enabled, "Pruning loose objects");
        let report = repository
            .prune_unreachable_loose(&mut isolation, &policy, cancel)
            .wrap_err("pruning loose objects")?;
        progress.finish(format_args!("{} objects", report.deleted.len()));
        writeln!(out, "pruned-loose {}", report.deleted.len())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Plans retention, failing unless the scan was complete.
fn plan(repository: &Repository, policy: &RetentionPolicy) -> Result<RetentionPlan> {
    let plan = repository.plan_retention(policy, &AtomicBool::new(false));
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
    let objects = repository.objects()?;
    let store = primary(&objects)?;
    let keep = |id: &ObjectId| !plan.reachable.contains(id) && !plan.required.contains(id);
    let mut loose: BTreeSet<_> = loose_ids(&store)?.into_iter().collect();
    loose.retain(keep);
    let mut packed: BTreeSet<_> = store.packs().flat_map(|pack| pack.ids()).collect();
    packed.retain(keep);
    Ok((loose, packed))
}

/// The repository's own object store, which excludes borrowed alternates.
fn primary(objects: &Objects) -> Result<ObjectStore<'_>> {
    objects
        .stores()
        .next()
        .ok_or_else(|| eyre!("object storage has no primary store"))
}

/// Multi-pack index inspection and explicit writing.
#[derive(Debug, clap::Subcommand)]
pub enum MultiPackIndexCommand {
    /// Write a monolithic index for primary-store packs. Exclude concurrent pack writers.
    /// With --git, prints no output on success, like git multi-pack-index write.
    Write {
        /// Prefer this pack checksum when an object appears in multiple packs.
        #[arg(long)]
        preferred_pack: Option<ObjectId>,
        /// Also write a reachability bitmap and reverse index for the complete packed graph.
        #[arg(long)]
        bitmap: bool,
    },
    /// Verify a standalone index or incremental chain against its named pack indexes.
    /// With --git, prints no output on success, like git multi-pack-index verify.
    Verify,
}

/// Runs explicit MIDX operations; automatic writing belongs to maintenance policy.
pub fn multi_pack_index(command: MultiPackIndexCommand, context: &Context) -> Result<ExitCode> {
    let repository = context.repository()?;
    match command {
        MultiPackIndexCommand::Write {
            preferred_pack,
            bitmap,
        } => {
            let mut options = girt::multi_pack_index::WriteOptions::default();
            options.preferred_pack = preferred_pack;
            let (checksum, objects, packs) = if bitmap {
                let report = repository.write_multi_pack_bitmap(
                    &mut UserAsserted,
                    options,
                    Default::default(),
                )?;
                (report.anchor, report.objects, report.packs)
            } else {
                let report = repository.write_multi_pack_index(&mut UserAsserted, options)?;
                (report.checksum, report.objects, report.packs)
            };
            if !context.git {
                writeln!(stdout(), "{} {} {}", checksum, objects, packs)?;
            }
        }
        MultiPackIndexCommand::Verify => {
            repository.verify_multi_pack_index()?;
            if !context.git {
                writeln!(stdout(), "ok")?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Options for verified removal of loose copies already stored in primary packs.
#[derive(Debug, clap::Args)]
pub struct PrunePackedArgs {
    /// Assert that no other writer, pack installer or dependent reader uses the repository.
    #[arg(long, required = true)]
    yes: bool,
    /// Maximum fanout directories and entries visited, including ignored names.
    #[arg(long, default_value_t = 1_000_000)]
    max_entries: usize,
    /// Maximum combined packed survivor payload bytes verified before deletion.
    #[arg(long, default_value_t = 1_073_741_824)]
    max_bytes: u64,
}

/// Removes verified loose duplicates under the user's explicit isolation assertion.
pub fn prune_packed(args: PrunePackedArgs, context: &Context) -> Result<ExitCode> {
    if !args.yes {
        bail!("--yes must assert repository-wide isolation");
    }
    let repository = context.repository()?;
    let mut limits = girt::retention::PrunePackedLimits::default();
    limits.max_entries = args.max_entries;
    limits.max_bytes = args.max_bytes;
    let report = repository
        .prune_packed(&mut UserAsserted, limits, &AtomicBool::new(false))
        .map_err(|failure| {
            let deleted = failure.report.deleted.len();
            let uncertain = failure.uncertain.map_or_else(
                || "none".to_owned(),
                |id| id.to_string(),
            );
            eyre!(failure).wrap_err(format!(
                "pruning verified loose duplicates: deleted {deleted}; uncertain loose object {uncertain}"
            ))
        })?;
    if !context.git {
        let mut out = stdout();
        writeln!(out, "deleted {}", report.deleted.len())?;
        writeln!(out, "retained {}", report.retained.len())?;
        writeln!(out, "verified-bytes {}", report.verified_bytes)?;
    }
    Ok(ExitCode::SUCCESS)
}
