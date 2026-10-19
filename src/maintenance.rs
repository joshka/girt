//! Caller-controlled automatic maintenance decisions.
//!
//! After a batch that creates objects, call [`Repository::check_auto_gc`] to decide whether
//! housekeeping is due. This observation never starts a process, prints a message, takes a
//! maintenance lock or changes storage. Applications such as jj retain control of latency and
//! background execution. A due result is not permission to delete objects: execution still needs
//! the isolation and retention policy described in [`crate::retention`].
//!
//! [`TaskSelection`] validates explicit tasks separately from execution and configuration. It
//! refuses an implicit default task set; selecting a task does not run maintenance.
//! [`MaintenanceConfig`] interprets individual settings lazily over a resolved configuration.
//! [`MaintenanceRun`] composes explicit pack-refs, commit-graph, loose-objects and Unix
//! incremental-repack tasks under caller isolation and a shared maintenance lock. It refuses other
//! tasks before any effects. [`PrefetchRefspecs`] prepares bounded cache mappings without selecting
//! a remote or transport.
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::maintenance::{AutoGc, CheckLimits};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Bare,
//! )?;
//! let decision = repo.check_auto_gc(CheckLimits::default(), &AtomicBool::new(false))?;
//! assert_eq!(decision, AutoGc::NotDue);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
mod config;
mod gc;
mod lock;
mod policy;
mod prefetch;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "prefetch execution follows separately")
)]
mod prefetch_config;
#[cfg(test)]
mod prefetch_config_tests;
mod run;
mod tasks;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, io};

pub use config::{AutoThreshold, MaintenanceConfig, MaintenanceStrategy, TaskConfigError};
pub use gc::{GcCoordinationError, GcLock, GcLog};
pub use lock::{LockError, MaintenanceLock};
pub use policy::PolicyError;
pub use prefetch::{
    PrefetchLimits, PrefetchRefspecs, PrefetchRefspecsError, PrefetchSkipReason,
    SkippedPrefetchSpec,
};
pub use run::{
    IncrementalRepackLimits, IncrementalRepackReport, LooseObjectsReport, MaintenanceFailure,
    MaintenanceLimits, MaintenanceReport, MaintenanceRun, MaintenanceRunError, TaskOutcome,
    TaskReport, TaskSkip,
};
pub use tasks::{MaintenanceTask, TaskSelection, TaskSelectionError};

use crate::{Config, Repository};

/// Bounds directory enumeration during a maintenance check.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CheckLimits {
    /// Maximum entries visited across the pack directory and loose-object sample combined.
    /// Each pack index can also require two sidecar metadata checks.
    pub max_entries: usize,
}

impl Default for CheckLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
        }
    }
}

/// A read-only observation of Git's automatic gc thresholds.
///
/// Results can become stale immediately. They do not inspect `gc.pid` or `gc.log`, acquire
/// isolation, or authorize consolidation. `maintenance.auto` controls a maintenance runner rather
/// than this direct `gc --auto` threshold check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AutoGc {
    /// `gc.auto` is zero or negative, disabling both automatic gc thresholds.
    Disabled,
    /// Neither enabled threshold was exceeded.
    NotDue,
    /// A threshold was exceeded. The application chooses whether and when to run maintenance.
    Due {
        /// The first threshold found exceeded; pack count is checked before loose objects.
        reason: GcReason,
        /// `gc.autoDetach` (default true), a preference for the application's scheduling policy.
        /// Girt never detaches or starts background work itself.
        detach: bool,
    },
}

/// Why automatic gc is due.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GcReason {
    /// Indexed primary packs without `.keep` exceeded `gc.autoPackLimit`.
    Packs,
    /// Correctly named loose entries in `objects/17` exceeded `gc.auto / 256` rounded up.
    LooseObjects,
}

/// A maintenance check could not finish; no repository files were changed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CheckError {
    /// A consulted gc setting is malformed or outside Git's signed 32-bit integer range.
    #[error("invalid automatic gc configuration")]
    Config(#[from] crate::config::InvalidValue),
    /// A directory or pack sidecar could not be inspected.
    #[error("cannot inspect automatic gc inputs")]
    Io(#[from] io::Error),
    /// The combined directory-entry budget was exhausted before a decision.
    #[error("automatic gc directory entry limit exceeded")]
    EntryLimit,
    /// Cancellation was observed before a decision.
    #[error("automatic gc check cancelled")]
    Cancelled,
}

impl Repository {
    /// Checks `gc.auto` and `gc.autoPackLimit` without running housekeeping.
    ///
    /// `gc.auto` defaults to 6700; nonpositive values disable both checks. Packs are counted in
    /// the primary store only, excluding `.keep` packs, and must exceed `gc.autoPackLimit`
    /// (default 50; nonpositive disables that check). Loose objects are estimated from correctly
    /// named entries in `objects/17`, using Git's strict sample threshold. Names are observed,
    /// not decoded; a due result does not certify object integrity. No other object fanout is read.
    /// Both SHA-1 and SHA-256 names are supported. The check stops once a threshold is exceeded.
    ///
    /// Call explicitly after commit, merge, fetch, am, rebase or receive batches when the
    /// application wants automatic housekeeping. No object-writing operation invokes this check.
    /// Applications can display an auto-packing message on `Due`, then arrange isolated work.
    /// `gc.autoDetach` is returned as a scheduling preference, never acted on by the library.
    ///
    /// # Errors
    ///
    /// Returns [`CheckError`] for invalid consulted settings, filesystem errors, cancellation or
    /// exhausted limits. No maintenance runs and no files are changed, including on failure.
    pub fn check_auto_gc(
        &self,
        limits: CheckLimits,
        cancel: &AtomicBool,
    ) -> Result<AutoGc, CheckError> {
        checkpoint(cancel)?;
        let config = self.config();
        let loose_limit = integer(config, "auto", 6700)?;
        let pack_limit = integer(config, "autoPackLimit", 50)?;
        let detach = config.boolean("gc", None, "autoDetach")?.unwrap_or(true);
        if loose_limit <= 0 {
            return Ok(AutoGc::Disabled);
        }
        let mut remaining = limits.max_entries;
        if pack_limit > 0 {
            let mut packs = 0;
            let directory = self.object_dir().join("pack");
            for entry in entries(&directory)? {
                visit(&mut remaining, cancel)?;
                let path = entry?.path();
                if path.extension().is_some_and(|extension| extension == "idx")
                    && path.with_extension("pack").try_exists()?
                    && !path.with_extension("keep").try_exists()?
                {
                    packs += 1;
                    if packs > pack_limit {
                        return Ok(AutoGc::Due {
                            reason: GcReason::Packs,
                            detach,
                        });
                    }
                }
            }
        }
        let threshold = (loose_limit as usize).div_ceil(256);
        let mut loose = 0;
        for entry in entries(&self.object_dir().join("17"))? {
            visit(&mut remaining, cancel)?;
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if name.len() == self.object_format().digest_len() * 2 - 2
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                loose += 1;
                if loose > threshold {
                    return Ok(AutoGc::Due {
                        reason: GcReason::LooseObjects,
                        detach,
                    });
                }
            }
        }
        checkpoint(cancel)?;
        Ok(AutoGc::NotDue)
    }
}

fn integer(config: &Config, key: &str, default: i64) -> Result<i32, CheckError> {
    let value = config.integer("gc", None, key)?.unwrap_or(default);
    i32::try_from(value).map_err(|_| {
        crate::config::InvalidValue::new("gc", key, config.value("gc", None, key).flatten()).into()
    })
}

fn entries(path: &Path) -> io::Result<impl Iterator<Item = io::Result<fs::DirEntry>>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries).into_iter().flatten()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None.into_iter().flatten()),
        Err(error) => Err(error),
    }
}

fn checkpoint(cancel: &AtomicBool) -> Result<(), CheckError> {
    if cancel.load(Ordering::Relaxed) {
        Err(CheckError::Cancelled)
    } else {
        Ok(())
    }
}

fn visit(remaining: &mut usize, cancel: &AtomicBool) -> Result<(), CheckError> {
    checkpoint(cancel)?;
    *remaining = remaining.checked_sub(1).ok_or(CheckError::EntryLimit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat};

    fn fixture(format: ObjectFormat, config: &str) -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let path = repo.common_dir().join("config");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(config.as_bytes());
        fs::write(path, bytes).unwrap();
        let repo = Repository::open(root.path().join("r")).unwrap();
        (root, repo)
    }

    fn check(repo: &Repository) -> AutoGc {
        repo.check_auto_gc(CheckLimits::default(), &AtomicBool::new(false))
            .unwrap()
    }

    #[rstest::rstest]
    #[case(ObjectFormat::Sha1)]
    #[case(ObjectFormat::Sha256)]
    fn loose_sample_strict_boundary(#[case] format: ObjectFormat) {
        let (_root, repo) = fixture(format, "\n[gc]\nauto=256\nautoDetach=false\n");
        let directory = repo.object_dir().join("17");
        fs::create_dir(&directory).unwrap();
        let length = format.digest_len() * 2 - 2;
        fs::write(
            directory.join(format!("{:0length$x}", 1)),
            b"threshold fixture",
        )
        .unwrap();
        assert_eq!(check(&repo), AutoGc::NotDue);
        fs::write(directory.join("invalid-name"), b"").unwrap();
        assert_eq!(check(&repo), AutoGc::NotDue);
        fs::write(
            directory.join(format!("{:0length$x}", 2)),
            b"threshold fixture",
        )
        .unwrap();
        assert_eq!(
            check(&repo),
            AutoGc::Due {
                reason: GcReason::LooseObjects,
                detach: false
            }
        );
    }

    // Independent Git 2.56 CLI oracle. Invalid payloads deliberately stop gc after selection;
    // the check observes names only and must never decode or rewrite them.
    #[rstest::rstest]
    #[case(ObjectFormat::Sha1)]
    #[case(ObjectFormat::Sha256)]
    fn sample_selection_matches_git(#[case] format: ObjectFormat) {
        let (root, repo) = fixture(format, "\n[gc]\nauto=256\nautoDetach=false\n");
        let directory = repo.object_dir().join("17");
        fs::create_dir(&directory).unwrap();
        for count in 1..=2 {
            let length = format.digest_len() * 2 - 2;
            fs::write(
                directory.join(format!("{count:0length$x}")),
                b"invalid threshold fixture",
            )
            .unwrap();
            let mut command = std::process::Command::new("git");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            let output = command
                .current_dir(repo.git_dir())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
                .env("GIT_AUTHOR_NAME", "Maintenance Fixture")
                .env("GIT_AUTHOR_EMAIL", "maintenance@example.com")
                .env("GIT_COMMITTER_NAME", "Maintenance Fixture")
                .env("GIT_COMMITTER_EMAIL", "maintenance@example.com")
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .args(["-c", "commit.gpgsign=false", "gc", "--auto"])
                .output()
                .unwrap();
            let selected = String::from_utf8_lossy(&output.stderr).contains("Auto packing");
            assert_eq!(selected, count == 2, "{output:?}");
            assert_eq!(matches!(check(&repo), AutoGc::Due { .. }), selected);
        }
    }

    #[test]
    fn packs_exclude_kept_and_unindexed() {
        let (_root, repo) = fixture(ObjectFormat::Sha1, "\n[gc]\nautoPackLimit=1\n");
        let directory = repo.object_dir().join("pack");
        fs::create_dir_all(&directory).unwrap();
        for (name, extensions) in [
            ("a", &["pack", "idx"][..]),
            ("b", &["pack", "idx", "keep"][..]),
            ("c", &["pack"][..]),
        ] {
            for ext in extensions {
                fs::write(directory.join(format!("pack-{name}.{ext}")), b"").unwrap();
            }
        }
        assert_eq!(check(&repo), AutoGc::NotDue);
        fs::remove_file(directory.join("pack-b.keep")).unwrap();
        assert_eq!(
            check(&repo),
            AutoGc::Due {
                reason: GcReason::Packs,
                detach: true
            }
        );
    }

    #[rstest::rstest]
    #[case("auto=0")]
    #[case("auto=-1")]
    fn disabled_thresholds(#[case] config: &str) {
        let (_root, repo) = fixture(ObjectFormat::Sha1, &format!("\n[gc]\n{config}\n"));
        assert_eq!(check(&repo), AutoGc::Disabled);
    }

    #[test]
    fn checks_are_bounded_and_cancellable() {
        let (_root, repo) = fixture(ObjectFormat::Sha1, "");
        fs::create_dir_all(repo.object_dir().join("17")).unwrap();
        fs::write(repo.object_dir().join("17/ignored"), b"").unwrap();
        assert!(matches!(
            repo.check_auto_gc(CheckLimits { max_entries: 0 }, &AtomicBool::new(false)),
            Err(CheckError::EntryLimit)
        ));
        assert!(matches!(
            repo.check_auto_gc(CheckLimits::default(), &AtomicBool::new(true)),
            Err(CheckError::Cancelled)
        ));
        assert!(!repo.common_dir().join("gc.pid").exists());
        assert!(!repo.common_dir().join("gc.log").exists());
    }

    #[rstest::rstest]
    #[case("auto=junk")]
    #[case("auto=2147483648")]
    #[case("autoPackLimit=-2147483649")]
    #[case("autoDetach=junk")]
    #[case("auto=0\nautoDetach=junk")]
    #[case("auto=0\nautoPackLimit=bad")]
    fn invalid_config_refused(#[case] config: &str) {
        let (_root, repo) = fixture(ObjectFormat::Sha1, &format!("\n[gc]\n{config}\n"));
        assert!(matches!(
            repo.check_auto_gc(CheckLimits::default(), &AtomicBool::new(false)),
            Err(CheckError::Config(_))
        ));
    }
}
