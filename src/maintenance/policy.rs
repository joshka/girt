use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Repository;
use crate::retention::{PackConsolidation, ReflogExpiry, ReflogExpiryPolicy, RetentionPolicy};

/// Why configured gc expiry could not be resolved; no files were changed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyError {
    /// The supplied clock predates the Unix epoch or a cutoff cannot fit `SystemTime`.
    #[error("gc clock or expiry cutoff cannot be represented")]
    Clock,
    /// `gc.pruneExpire` is malformed or uses unsupported calendar/natural-language syntax.
    #[error("unsupported gc prune expiry")]
    Prune(#[source] crate::config::InvalidValue),
    /// Reflog expiry configuration is invalid or exceeds pattern limits.
    #[error("cannot resolve gc reflog expiry")]
    Reflog(#[from] crate::retention::ReflogExpiryError),
}

impl Repository {
    /// Resolves Git's gc expiry settings into an explicit retention policy.
    ///
    /// This opts into reclamation policy but changes no files. It replaces `recent_cutoff` and
    /// `reflog_expiry` in `policy`, preserving its caller-owned heads and resource limits.
    /// `gc.pruneExpire` defaults to two weeks; reflog expiry defaults to 90 days for all records
    /// and 30 days for disconnected records. `never` disables its rule. Numeric Unix timestamps,
    /// second/minute/hour/day/week intervals use the same date parser as [`ReflogExpiry`];
    /// unsupported calendar/natural-language expressions refuse the policy. For loose-object
    /// pruning, `now` and `all` resolve to the supplied clock, conservatively preserving
    /// future-dated objects. Reflog rules retain their documented maximum-timestamp semantics.
    ///
    /// Pack consolidation is disabled in the returned policy. A caller can explicitly select
    /// [`PackConsolidation::Configured`] to consolidate only above `gc.autoPackLimit`; resolving
    /// expiry never grants implicit permission to rewrite packs. Bitmap writing remains opt-in
    /// through [`crate::retention::RepackLimits`].
    ///
    /// Run the resulting policy with [`Self::collect_garbage_with`] under caller-provided
    /// [`crate::retention::MaintenanceIsolation`]. The existing primitives protect recent packs,
    /// `.keep` packs and alternates conservatively; configured expiry is not exact Git gc parity.
    /// In particular, reflog disconnected classification uses repository-wide live roots.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] before any mutation when the clock or any consulted expiry value
    /// is invalid or unsupported.
    ///
    /// ```
    /// use std::time::{Duration, UNIX_EPOCH};
    ///
    /// use girt::retention::{PackConsolidation, RetentionPolicy};
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    /// let policy = repo.configured_gc_policy(now, RetentionPolicy::default())?;
    /// assert_eq!(
    ///     policy.recent_cutoff,
    ///     Some(now - Duration::from_secs(14 * 86400))
    /// );
    /// assert_eq!(policy.pack_consolidation, PackConsolidation::Never);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn configured_gc_policy(
        &self,
        now: SystemTime,
        mut policy: RetentionPolicy,
    ) -> Result<RetentionPolicy, PolicyError> {
        let seconds = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PolicyError::Clock)?
            .as_secs();
        let value = match self.config().value("gc", None, "pruneExpire") {
            None => Some(&b"2 weeks ago"[..]),
            Some(value) => value,
        };
        let cutoff = value
            .and_then(|value| crate::retention::parse_expiry_date(value, i128::from(seconds)))
            .ok_or_else(|| {
                PolicyError::Prune(crate::config::InvalidValue::new("gc", "pruneExpire", value))
            })?;
        policy.recent_cutoff = cutoff
            .map(|seconds| {
                if seconds == i128::from(u64::MAX) {
                    return Ok(now);
                }
                let seconds = u64::try_from(seconds).map_err(|_| PolicyError::Clock)?;
                UNIX_EPOCH
                    .checked_add(Duration::from_secs(seconds))
                    .ok_or(PolicyError::Clock)
            })
            .transpose()?;
        policy.reflog_expiry = ReflogExpiryPolicy::Configured(ReflogExpiry::from_config(
            self.config(),
            i128::from(seconds),
        )?);
        policy.pack_consolidation = PackConsolidation::Never;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refs::RefName;

    fn fixture(config: &str) -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("r"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let path = repo.common_dir().join("config");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(config.as_bytes());
        std::fs::write(path, bytes).unwrap();
        let repo = Repository::open(repo.git_dir()).unwrap();
        (root, repo)
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn defaults_preserve_caller_bounds_and_disable_consolidation() {
        let (_root, repo) = fixture("");
        let input = RetentionPolicy::default().with_max_entries(42);
        let policy = repo.configured_gc_policy(now(), input).unwrap();
        assert_eq!(policy.max_entries, 42);
        assert_eq!(
            policy.recent_cutoff,
            Some(now() - Duration::from_secs(14 * 86400))
        );
        assert_eq!(policy.pack_consolidation, PackConsolidation::Never);
        let ReflogExpiryPolicy::Configured(expiry) = policy.reflog_expiry else {
            panic!("configured expiry expected")
        };
        let cutoffs = expiry
            .cutoffs(&RefName::new("refs/heads/main").unwrap())
            .unwrap();
        assert_eq!(cutoffs.general, Some(1_800_000_000 - 90 * 86400));
        assert_eq!(cutoffs.unreachable, Some(1_800_000_000 - 30 * 86400));
    }

    #[rstest::rstest]
    #[case("never", None)]
    #[case("false", None)]
    #[case("now", Some(1_800_000_000))]
    #[case("all", Some(1_800_000_000))]
    #[case("7 days ago", Some(1_800_000_000 - 7 * 86400))]
    #[case("1700000000", Some(1_700_000_000))]
    fn configured_prune_cutoff(#[case] value: &str, #[case] expected: Option<u64>) {
        let (_root, repo) = fixture(&format!("\n[gc]\npruneExpire={value}\n"));
        let policy = repo
            .configured_gc_policy(now(), RetentionPolicy::default())
            .unwrap();
        assert_eq!(
            policy.recent_cutoff,
            expected.map(|s| UNIX_EPOCH + Duration::from_secs(s))
        );
    }

    #[rstest::rstest]
    #[case("pruneExpire=next month")]
    #[case("pruneExpire")]
    #[case("reflogExpire=unsupported")]
    #[case("reflogExpireUnreachable=unsupported")]
    fn invalid_expiry_refuses_before_execution(#[case] config: &str) {
        let (_root, repo) = fixture(&format!("\n[gc]\n{config}\n"));
        assert!(
            repo.configured_gc_policy(now(), RetentionPolicy::default())
                .is_err()
        );
        assert!(!repo.common_dir().join("gc.pid").exists());
    }
}
