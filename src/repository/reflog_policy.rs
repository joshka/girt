//! Git's `core.logAllRefUpdates` policy for deciding which reference updates get reflog records.
use thiserror::Error;

use super::Repository;
use crate::Signature;
use crate::config::InvalidValue;
use crate::refs::{RefName, ReferenceError, Reflog};

/// A reflog policy could not be decided.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReflogPolicyError {
    /// `core.logAllRefUpdates` is not a boolean or `always`.
    #[error("invalid reflog configuration")]
    Config(#[source] InvalidValue),
    /// Checking for an existing log failed.
    #[error("checking for an existing reflog")]
    Reference(#[source] ReferenceError),
}

impl Repository {
    /// Returns whether Git would record an update of `name` in its reflog.
    ///
    /// Follows `core.logAllRefUpdates` as observed with `git update-ref`:
    ///
    /// - `always` (any case) logs every reference.
    /// - `true`, or no setting in a non-bare repository, logs `HEAD` and references under
    ///   `refs/heads/`, `refs/remotes/` and `refs/notes/`.
    /// - `false`, or no setting in a bare repository, creates no logs.
    ///
    /// A reference that already has a log is always logged, whatever the setting. Callers pass
    /// [`crate::refs::Reflog::AppendIfChanged`] when this returns `true` and
    /// [`crate::refs::Reflog::Preserve`] otherwise; [`Self::reflog_for`] makes that choice.
    ///
    /// # Errors
    ///
    /// Returns [`ReflogPolicyError::Config`] for a value Git rejects, including a variable
    /// without `=`, and [`ReflogPolicyError::Reference`] when the existing log can't be checked.
    pub fn logs_updates_to(&self, name: &RefName) -> Result<bool, ReflogPolicyError> {
        let config = self.config();
        let enabled = match config.value("core", None, "logallrefupdates") {
            None => !self.is_bare(),
            // Git rejects the implicit `true` of a `logAllRefUpdates` line without a value.
            Some(None) => {
                return Err(ReflogPolicyError::Config(InvalidValue::new(
                    "core",
                    "logallrefupdates",
                    None,
                )));
            }
            Some(Some(value)) if value.eq_ignore_ascii_case(b"always") => return Ok(true),
            Some(Some(_)) => config
                .boolean("core", None, "logallrefupdates")
                .map_err(ReflogPolicyError::Config)?
                .unwrap_or(false),
        };
        let bytes = name.as_bytes();
        let conventional = bytes == b"HEAD"
            || [&b"refs/heads/"[..], b"refs/remotes/", b"refs/notes/"]
                .iter()
                .any(|prefix| bytes.starts_with(prefix));
        if enabled && conventional {
            return Ok(true);
        }
        self.references()
            .and_then(|references| references.has_reflog(name))
            .map_err(ReflogPolicyError::Reference)
    }

    /// Chooses the reflog policy Git would use for an update of `name`.
    ///
    /// Returns [`Reflog::AppendIfChanged`] with `committer` and `message` when
    /// [`Self::logs_updates_to`] returns `true`, and [`Reflog::Preserve`] otherwise. Like
    /// `git update-ref -m <message>`, an update that leaves the value unchanged records nothing.
    ///
    /// The policy applies to every name a [`crate::refs::Dereference::Follow`] edit logs. Git
    /// decides per name, so pass the chain's terminal name (from
    /// [`crate::refs::References::resolve`]) when editing through an alias: a branch reached
    /// through a symbolic name without a log of its own is then still logged.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::logs_updates_to`].
    ///
    /// ```
    /// use girt::refs::{RefEdit, RefName, Reflog};
    /// use girt::{InitKind, ObjectFormat, Repository, Signature};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("work"),
    ///     InitKind::Worktree,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"tip")?;
    /// let who = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"writer@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    ///
    /// // A non-bare repository logs branches but not tags by default.
    /// let branch = RefName::branch("topic")?;
    /// let reflog = repo.reflog_for(&branch, who.clone(), "branch: Created")?;
    /// assert!(matches!(reflog, Reflog::AppendIfChanged { .. }));
    /// assert_eq!(
    ///     repo.reflog_for(&RefName::tag("v1")?, who, "tag")?,
    ///     Reflog::Preserve
    /// );
    ///
    /// let refs = repo.references()?;
    /// refs.transaction(&[RefEdit::create(branch.clone(), id, reflog)])?;
    /// assert_eq!(refs.reflog(&branch)?.map(|log| log.len()), Some(1));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn reflog_for(
        &self,
        name: &RefName,
        committer: Signature,
        message: impl Into<Vec<u8>>,
    ) -> Result<Reflog, ReflogPolicyError> {
        Ok(if self.logs_updates_to(name)? {
            Reflog::AppendIfChanged {
                committer,
                message: message.into(),
            }
        } else {
            Reflog::Preserve
        })
    }
}
