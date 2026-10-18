//! Which host keys an SSH connection trusts, enforced by OpenSSH.

use std::path::PathBuf;

use super::SshError;

/// How OpenSSH checks the server's host key, set with [`SshRemote::with_host_keys`].
///
/// girt runs OpenSSH, which checks host keys itself before girt sees the connection, so the
/// policy is fixed before connecting rather than decided per key. It becomes OpenSSH options that
/// take precedence over the user's `~/.ssh/config`:
///
/// - [`Self::files`] become `UserKnownHostsFile`, and `GlobalKnownHostsFile` is turned off, so only
///   those files are trusted. Empty keeps OpenSSH's own files.
/// - [`Self::unknown`] becomes `StrictHostKeyChecking`: `yes` for [`UnknownHost::Reject`] and
///   `accept-new` for [`UnknownHost::AcceptNew`]. A changed key is always refused.
///
/// `UpdateHostKeys` is turned off, so OpenSSH never rewrites the files on its own.
///
/// [`SshRemote::with_host_keys`]: super::SshRemote::with_host_keys
///
/// # Examples
///
/// ```
/// use girt::transport::ssh::{HostKeyPolicy, UnknownHost};
///
/// let policy =
///     HostKeyPolicy::new(UnknownHost::AcceptNew).with_files(vec!["/srv/app/known_hosts".into()]);
/// assert_eq!(policy.unknown, UnknownHost::AcceptNew);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostKeyPolicy {
    /// The `known_hosts` files to trust. Empty keeps OpenSSH's own files.
    pub files: Vec<PathBuf>,
    /// What happens when the server's key is in none of the files.
    pub unknown: UnknownHost,
}

/// What OpenSSH does with a host whose key is not known yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnknownHost {
    /// Refuse the connection (`StrictHostKeyChecking=yes`).
    Reject,
    /// Trust the key and add it to the first file (`StrictHostKeyChecking=accept-new`).
    AcceptNew,
}

impl HostKeyPolicy {
    /// A policy that uses OpenSSH's own `known_hosts` files.
    pub fn new(unknown: UnknownHost) -> Self {
        Self {
            files: Vec::new(),
            unknown,
        }
    }

    /// Sets [`Self::files`].
    #[must_use]
    pub fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.files = files;
        self
    }

    // The `-o` values, in the order they must come before any other option.
    pub(super) fn options(&self) -> Result<Vec<String>, SshError> {
        let mut options = vec![
            format!(
                "StrictHostKeyChecking={}",
                match self.unknown {
                    UnknownHost::Reject => "yes",
                    UnknownHost::AcceptNew => "accept-new",
                }
            ),
            "UpdateHostKeys=no".to_owned(),
        ];
        if !self.files.is_empty() {
            let mut files = Vec::new();
            for file in &self.files {
                let file = file
                    .to_str()
                    .filter(|file| {
                        !file.is_empty()
                            && !file.contains(['"', '\0', '\n', '\r'])
                            && std::path::Path::new(file).is_absolute()
                    })
                    .ok_or(SshError::Configuration("known_hosts path"))?;
                // OpenSSH expands `%` tokens and a leading `~` in these paths; keep them literal.
                files.push(format!("\"{}\"", file.replace('%', "%%")));
            }
            options.push(format!("UserKnownHostsFile={}", files.join(" ")));
            options.push("GlobalKnownHostsFile=none".to_owned());
        }
        Ok(options)
    }
}
