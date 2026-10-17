//! `@{upstream}` and `@{push}`: remote-tracking branches derived from branch configuration.

use super::{Resolver, RevisionError};
use crate::ObjectId;
use crate::refs::RefName;
use crate::remote::{ConfiguredRefspec, ConfiguredRefspecKind, Direction};

impl Resolver<'_> {
    /// `branch@{upstream}`: `branch.<b>.merge` mapped through `branch.<b>.remote`'s fetch
    /// refspecs, or the local branch itself when the remote is `.`.
    pub(super) fn upstream(&self, branch: Option<&str>) -> Result<ObjectId, RevisionError> {
        let branch = self.branch(branch)?;
        let tracking = self.upstream_ref(&branch)?;
        self.resolve_ref(&tracking)
    }

    /// `branch@{push}`: where `git push` would send the branch, as a remote-tracking branch.
    ///
    /// The push remote is `branch.<b>.pushRemote`, then `remote.pushDefault`, then
    /// `branch.<b>.remote`, then the only configured remote or `origin`. `push.default` selects the
    /// remote branch: `upstream` uses the upstream; `simple` uses it only when pushing to the
    /// upstream's remote and the names match, and is refused otherwise as Git refuses it;
    /// `current` and `matching` use the same branch name on the push remote.
    pub(super) fn push_destination(&self, branch: Option<&str>) -> Result<ObjectId, RevisionError> {
        let branch = self.branch(branch)?;
        let short = short_branch(&branch);
        let config = self.repository.config();
        let no_push = || RevisionError::NoUpstream {
            branch: String::from_utf8_lossy(short).into_owned(),
            kind: "push destination",
        };
        let fetch_remote = config.string("branch", Some(short), "remote");
        let remote = config
            .string("branch", Some(short), "pushremote")
            .or_else(|| config.string("remote", None, "pushdefault"))
            .or(fetch_remote)
            .or_else(|| default_remote(config))
            .ok_or_else(no_push)?;
        if config.string("remote", Some(remote), "push").is_some() {
            return Err(RevisionError::Unsupported(
                "`@{push}` with configured push refspecs",
            ));
        }
        let mode = config
            .string("push", None, "default")
            .unwrap_or(b"simple")
            .to_ascii_lowercase();
        let same_remote = fetch_remote == Some(remote);
        let tracking = match mode.as_slice() {
            b"upstream" | b"tracking" => self.upstream_ref(&branch)?,
            b"simple" if same_remote => {
                let upstream = config
                    .string("branch", Some(short), "merge")
                    .ok_or_else(no_push)?;
                if upstream != branch.as_bytes() {
                    return Err(no_push());
                }
                self.upstream_ref(&branch)?
            }
            b"current" | b"matching" => {
                tracking_ref(self, remote, branch.as_bytes()).ok_or_else(no_push)?
            }
            _ => return Err(no_push()),
        };
        self.resolve_ref(&tracking)
    }

    /// The named branch, or the current branch.
    fn branch(&self, name: Option<&str>) -> Result<RefName, RevisionError> {
        let Some(name) = name else {
            return self.current_branch();
        };
        let full = if name.starts_with("refs/heads/") {
            name.to_owned()
        } else {
            format!("refs/heads/{name}")
        };
        RefName::new(&full).map_err(|_| RevisionError::NotFound(name.to_owned()))
    }

    fn upstream_ref(&self, branch: &RefName) -> Result<RefName, RevisionError> {
        let short = short_branch(branch);
        let config = self.repository.config();
        let no_upstream = || RevisionError::NoUpstream {
            branch: String::from_utf8_lossy(short).into_owned(),
            kind: "upstream",
        };
        let remote = config
            .string("branch", Some(short), "remote")
            .ok_or_else(no_upstream)?;
        let merge = config
            .string("branch", Some(short), "merge")
            .ok_or_else(no_upstream)?;
        if remote == b"." {
            return RefName::new(merge).map_err(|_| no_upstream());
        }
        tracking_ref(self, remote, merge).ok_or_else(no_upstream)
    }
}

fn short_branch(branch: &RefName) -> &[u8] {
    branch
        .as_bytes()
        .strip_prefix(b"refs/heads/")
        .unwrap_or(branch.as_bytes())
}

/// Maps a remote's ref name through its fetch refspecs to the local tracking ref; the first
/// matching mapping wins, and an exclusion that matches suppresses the result.
fn tracking_ref(resolver: &Resolver<'_>, remote: &[u8], source: &[u8]) -> Option<RefName> {
    if remote == b"." {
        return RefName::new(source).ok();
    }
    let mut destination = None;
    for value in resolver
        .repository
        .config()
        .values("remote", Some(remote), "fetch")
        .flatten()
    {
        let Ok(spec) = ConfiguredRefspec::parse(Direction::Fetch, value) else {
            continue;
        };
        let Some(pattern) = spec.source() else {
            continue;
        };
        match spec.kind() {
            ConfiguredRefspecKind::Exclusion if glob(pattern, source).is_some() => return None,
            ConfiguredRefspecKind::Mapping => {
                if let (Some(matched), Some(target)) = (glob(pattern, source), spec.destination())
                    && destination.is_none()
                {
                    destination = Some(substitute(target, matched));
                }
            }
            _ => {}
        }
    }
    RefName::new(destination?).ok()
}

/// Matches `name` against a refspec side with at most one `*`, returning the text `*` matched.
fn glob<'n>(pattern: &[u8], name: &'n [u8]) -> Option<&'n [u8]> {
    let Some(star) = pattern.iter().position(|&b| b == b'*') else {
        return (pattern == name).then_some(&name[..0]);
    };
    let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
    (name.len() >= prefix.len() + suffix.len()
        && name.starts_with(prefix)
        && name.ends_with(suffix))
    .then(|| &name[prefix.len()..name.len() - suffix.len()])
}

fn substitute(target: &[u8], matched: &[u8]) -> Vec<u8> {
    match target.iter().position(|&b| b == b'*') {
        Some(star) => [&target[..star], matched, &target[star + 1..]].concat(),
        None => target.to_vec(),
    }
}

/// The remote `git push` uses when nothing names one: the only configured remote, else `origin`
/// when it is configured.
fn default_remote(config: &crate::Config) -> Option<&[u8]> {
    let remotes = config.subsection_names("remote");
    match remotes.as_slice() {
        [only] => Some(only),
        _ => remotes.into_iter().find(|name| *name == b"origin"),
    }
}
