//! Git's `core.logAllRefUpdates` policy for deciding which reference updates get reflog records.
use thiserror::Error;

use super::Repository;
use crate::config::InvalidValue;
use crate::refs::{RefName, ReferenceError};

/// A reflog policy could not be decided.
#[derive(Debug, Error)]
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
    /// [`crate::refs::Reflog::Append`] when this returns `true` and
    /// [`crate::refs::Reflog::Preserve`] otherwise.
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
}
