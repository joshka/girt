//! Fetching from and pushing to configured remotes.
//!
//! This module is the ordinary entry point for talking to another repository. It resolves a
//! named remote's URL through the repository configuration (`insteadOf`, `pushurl`, protocol
//! policy), selects the transport (local path, HTTP(S) or SSH), handles credentials the way Git
//! does, and drives the lower-level [`crate::fetch`] and [`crate::push`] workflows to completion.
//!
//! Process state that Git consults (environment variables such as `GIT_SSH_COMMAND`, proxies,
//! `HOME`) is passed explicitly as an [`Environment`]; use [`Environment::from_process`] for the
//! usual behavior.
//!
//! ```no_run
//! use girt::Repository;
//! use girt::transfer::{Environment, FetchOptions, NoCallbacks};
//!
//! let repo = Repository::open("project")?;
//! let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]).prune(true);
//! let outcome = repo.fetch(
//!     "origin",
//!     &options,
//!     &Environment::from_process(),
//!     &mut NoCallbacks,
//! )?;
//! println!("{} references updated", outcome.updated.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::fetch::{
    FetchLimits, FetchRequest, FetchUpdateKind, FetchUpdateLimits, FetchWorkflowError,
    KnownHistory, RemoteHead,
};
use crate::push::{PreparedPush, PushCommand, PushLimits, PushReport};
use crate::refs::{RefName, Reflog};
use crate::remote::{Destination, Direction, Protocol, ProtocolEnvironment, Refspecs, Remote};
use crate::transport::TransportControl;
use crate::{ObjectId, Repository};

/// A snapshot of the environment variables Git consults while transferring.
///
/// Transfers never read the process environment themselves. Child processes (SSH, credential
/// helpers) receive exactly these variables.
#[derive(Clone, Default)]
pub struct Environment {
    vars: BTreeMap<OsString, OsString>,
    current_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Values can contain secrets such as tokens.
        f.debug_struct("Environment")
            .field("names", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Environment {
    /// Captures the current process environment and working directory.
    pub fn from_process() -> Self {
        let mut environment: Self = std::env::vars_os().collect();
        environment.current_dir = std::env::current_dir().ok();
        environment
    }

    /// Sets the directory that relative local remote paths are resolved against, as Git resolves
    /// them against its working directory.
    pub fn with_current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(directory.into());
        self
    }

    /// Returns a variable's value.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    /// Sets a variable, returning `self` for chaining.
    pub fn with(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Removes a variable, returning `self` for chaining.
    pub fn without(mut self, name: impl AsRef<OsStr>) -> Self {
        self.vars.remove(name.as_ref());
        self
    }

    fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.get(name)
            .map(|value| value.as_encoded_bytes().to_vec())
    }

    fn string(&self, name: &str) -> Option<String> {
        self.get(name).and_then(OsStr::to_str).map(str::to_owned)
    }
}

impl<K: Into<OsString>, V: Into<OsString>> FromIterator<(K, V)> for Environment {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            vars: iter
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
            current_dir: None,
        }
    }
}

/// A value requested from the user while authenticating.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CredentialPrompt {
    /// The username for the URL.
    Username,
    /// The password or token for the URL.
    Password,
}

/// Hooks for reporting progress and interacting with the user during a transfer.
///
/// All methods have no-op defaults. Callbacks run on the calling thread and should return
/// promptly.
pub trait TransferCallbacks {
    /// Receives a progress or informational message from the remote, as raw bytes. Messages may
    /// end in `\r` (to be overwritten) or `\n`.
    fn remote_message(&mut self, _message: &[u8]) {}

    /// Receives diagnostic output of a local transport process, such as SSH's standard error
    /// (e.g. host key warnings or authentication failures), as raw bytes.
    fn transport_message(&mut self, _message: &[u8]) {}

    /// Asks the user for a credential after configured helpers didn't supply one. Returns
    /// `None` to cancel. `url` has no embedded password.
    fn credential(&mut self, _url: &str, _prompt: CredentialPrompt) -> Option<Vec<u8>> {
        None
    }
}

/// Callbacks that report nothing and never prompt.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCallbacks;

impl TransferCallbacks for NoCallbacks {}

/// What to fetch and how to update local references.
#[derive(Clone, Debug)]
pub struct FetchOptions {
    refspecs: Vec<Vec<u8>>,
    depth: Option<NonZeroU32>,
    prune: bool,
    tag_namespaces: Vec<RefName>,
    reflog: Option<(crate::Signature, Vec<u8>)>,
}

impl FetchOptions {
    /// Fetches with the given refspecs (positive, negative `^src`, or globs).
    pub fn new<I, S>(refspecs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        Self {
            refspecs: refspecs
                .into_iter()
                .map(|refspec| refspec.as_ref().to_vec())
                .collect(),
            depth: None,
            prune: false,
            tag_namespaces: Vec::new(),
            reflog: None,
        }
    }

    /// Limits history to `depth` commits from each fetched tip, creating or adjusting shallow
    /// boundaries.
    pub fn depth(mut self, depth: Option<NonZeroU32>) -> Self {
        self.depth = depth;
        self
    }

    /// Deletes destinations whose sources no longer exist on the remote, including destinations
    /// of exact refspecs whose source is missing.
    pub fn prune(mut self, prune: bool) -> Self {
        self.prune = prune;
        self
    }

    /// Permits destinations below `namespace` that hold tags (e.g. a per-remote tag namespace).
    ///
    /// Such destinations are force-updatable and pruned like remote-tracking references.
    pub fn tag_namespace(mut self, namespace: RefName) -> Self {
        self.tag_namespaces.push(namespace);
        self
    }

    /// Records reflog entries by `committer` with `message` for changed references. Without
    /// this, no reflog entries are written.
    pub fn reflog(mut self, committer: crate::Signature, message: impl Into<Vec<u8>>) -> Self {
        self.reflog = Some((committer, message.into()));
        self
    }
}

/// One local reference changed by a fetch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedRef {
    /// Local reference name.
    pub name: RefName,
    /// Previous value, if the reference existed.
    pub old: Option<ObjectId>,
    /// New value, or `None` if the reference was pruned.
    pub new: Option<ObjectId>,
}

/// The result of a successful fetch.
#[derive(Clone, Debug, Default)]
pub struct FetchOutcome {
    /// Local references that were created, updated or pruned.
    pub updated: Vec<FetchedRef>,
    /// Exact (non-glob) refspec sources that the remote doesn't have.
    pub missing_sources: Vec<Vec<u8>>,
}

/// Options for [`Repository::push`].
#[derive(Clone, Debug, Default)]
pub struct PushOptions {
    push_options: Vec<Vec<u8>>,
    identity: Option<crate::Signature>,
}

impl PushOptions {
    /// Identity for reflog entries written in a local destination repository.
    pub fn identity(mut self, identity: crate::Signature) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Sends server-specific `--push-option` values (e.g. Gerrit review settings).
    pub fn push_option(mut self, option: impl Into<Vec<u8>>) -> Self {
        self.push_options.push(option.into());
        self
    }
}

/// How the remote's HEAD is configured, as advertised.
pub type RemoteHeadState = RemoteHead;

/// A transfer failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// No remote with this name, or it has no URL for the direction.
    #[error("no such remote: {0}")]
    NoSuchRemote(String),
    /// The remote configuration is invalid.
    #[error("invalid remote configuration: {0}")]
    Configuration(String),
    /// The URL uses a transport girt doesn't support (e.g. `git://` or a remote helper).
    #[error("unsupported transport for remote {0}")]
    UnsupportedTransport(String),
    /// Authentication failed or was cancelled.
    #[error("authentication failed for {0}")]
    Authentication(String),
    /// A fetch failed before any local change.
    #[error(transparent)]
    Fetch(Box<FetchWorkflowError>),
    /// A fetch installed objects but failed to update some references.
    #[error(transparent)]
    FetchFinish(Box<crate::fetch::FetchFinishError>),
    /// A push failed; see [`crate::push::PushError`] for whether it may have taken effect.
    #[error(transparent)]
    Push(Box<crate::push::PushError>),
    /// Reading local objects or references failed.
    #[error(transparent)]
    Local(Box<dyn std::error::Error + Send + Sync>),
}

impl TransferError {
    fn local(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Local(error.into())
    }
}

/// A resolved connection target.
enum Endpoint {
    Local(PathBuf),
    #[cfg(feature = "http")]
    Http(Box<Http>),
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    Ssh(Box<crate::transport::ssh::SshRemote>),
}

#[cfg(feature = "http")]
struct Http {
    settings_config: crate::Config,
    remote_name: Vec<u8>,
    destination: Destination,
    environment: crate::transport::http::HttpEnvironment,
    helpers: Vec<crate::remote::CredentialHelper>,
    /// URL for prompts, without userinfo.
    display_url: String,
}

#[cfg(feature = "http")]
impl Http {
    fn remote(
        &self,
        credentials: Option<crate::remote::CredentialSession>,
    ) -> Result<crate::transport::http::HttpRemote, TransferError> {
        let settings = crate::transport::http::HttpSettings::resolve_for_remote(
            &self.settings_config,
            &self.remote_name,
            &self.destination,
            &self.environment,
        )
        .map_err(|error| TransferError::Configuration(error.to_string()))?;
        let remote = crate::transport::http::HttpRemote::configured(settings)
            .map_err(|error| TransferError::Configuration(error.to_string()))?;
        match credentials {
            Some(session) => remote
                .with_credentials(session)
                .map_err(|error| TransferError::Configuration(error.to_string())),
            None => Ok(remote),
        }
    }

    fn credentials(
        &self,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<crate::remote::CredentialSession, TransferError> {
        let url = url_parts(self.destination.bytes())
            .ok_or_else(|| TransferError::Configuration("invalid HTTP URL".into()))?;
        let context = crate::remote::CredentialContext {
            protocol: url.scheme.into_bytes(),
            host: url.authority.into_bytes(),
            path: Some(url.path.trim_matches('/').as_bytes().to_vec()),
            use_http_path: true,
        };
        let display_url = self.display_url.clone();
        let mut prompt = |prompt: crate::remote::Prompt| {
            let prompt = match prompt {
                crate::remote::Prompt::Username => CredentialPrompt::Username,
                crate::remote::Prompt::Password => CredentialPrompt::Password,
            };
            callbacks.credential(&display_url, prompt)
        };
        crate::remote::CredentialSession::fill(
            &self.settings_config,
            context,
            &self.helpers,
            Some(&mut prompt),
            &AtomicBool::new(false),
            None,
        )
        .map_err(|_| TransferError::Authentication(self.display_url.clone()))
    }
}

struct UrlParts {
    scheme: String,
    authority: String,
    path: String,
}

/// Splits `scheme://[user@]host[:port]/path` without percent decoding.
fn url_parts(bytes: &[u8]) -> Option<UrlParts> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (scheme, rest) = text.split_once("://")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(UrlParts {
        scheme: scheme.to_ascii_lowercase(),
        authority: authority.to_owned(),
        path: path.to_owned(),
    })
}

fn redacted_url(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match url_parts(bytes) {
        Some(parts) => format!("{}://{}{}", parts.scheme, parts.authority, parts.path),
        None => text.into_owned(),
    }
}

/// Quotes `value` for a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Finds `program` in the environment's `PATH`.
fn find_program(environment: &Environment, program: &str) -> Option<PathBuf> {
    let path = environment.get("PATH")?;
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Maps configured `credential.helper` values to programs, following Git's rules: `!cmd` is a
/// shell snippet, an absolute path is a shell command, and anything else names
/// `git-credential-<name>` (with any arguments).
#[cfg(feature = "http")]
fn credential_helpers(
    config: &crate::Config,
    environment: &Environment,
) -> Vec<crate::remote::CredentialHelper> {
    let exec_dirs: Vec<PathBuf> = environment
        .get("GIT_EXEC_PATH")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/opt/homebrew/opt/git/libexec/git-core",
                "/usr/local/opt/git/libexec/git-core",
                "/usr/lib/git-core",
                "/usr/libexec/git-core",
                "/Library/Developer/CommandLineTools/usr/libexec/git-core",
                "/Applications/Xcode.app/Contents/Developer/usr/libexec/git-core",
            ]
            .map(PathBuf::from),
        )
        .collect();
    config
        .entries()
        .iter()
        .filter(|entry| {
            entry.section.eq_ignore_ascii_case(b"credential")
                && entry.name.eq_ignore_ascii_case(b"helper")
        })
        .filter_map(|entry| entry.value.as_deref())
        .filter(|value| !value.is_empty())
        .filter_map(|value| {
            let text = std::str::from_utf8(value).ok()?;
            let command = if let Some(snippet) = text.strip_prefix('!') {
                snippet.to_owned()
            } else if Path::new(text.split_whitespace().next()?).is_absolute() {
                text.to_owned()
            } else {
                let (name, arguments) = text.split_once(' ').unwrap_or((text, ""));
                let program = format!("git-credential-{name}");
                let path = find_program(environment, &program).or_else(|| {
                    exec_dirs
                        .iter()
                        .map(|dir| dir.join(&program))
                        .find(|candidate| candidate.is_file())
                })?;
                format!("{} {arguments}", shell_quote(&path.to_string_lossy()))
            };
            Some(crate::remote::CredentialHelper {
                configured_name: value.to_vec(),
                program: crate::remote::CredentialProgram {
                    executable: PathBuf::from("/bin/sh"),
                    arguments: vec![
                        "-c".into(),
                        format!("{command} \"$@\"").into(),
                        command.into(),
                    ],
                    inherit_environment: false,
                    environment: environment
                        .vars
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect(),
                },
            })
        })
        .collect()
}

#[cfg(feature = "http")]
fn http_environment(
    config: &crate::Config,
    destination: &Destination,
    environment: &Environment,
) -> Result<crate::transport::http::HttpEnvironment, TransferError> {
    let ca_path = match environment.get("GIT_SSL_CAINFO") {
        Some(path) => Some(PathBuf::from(path)),
        None => crate::transport::http::HttpSettings::configured_ca_info(config, destination)
            .map_err(|error| TransferError::Configuration(error.to_string()))?
            .map(|path| {
                let path = String::from_utf8_lossy(path).into_owned();
                match (path.strip_prefix("~/"), environment.get("HOME")) {
                    (Some(rest), Some(home)) => Path::new(home).join(rest),
                    _ => PathBuf::from(path),
                }
            }),
    };
    let ssl_ca_info = ca_path
        .map(|path| {
            std::fs::read(&path).map_err(|error| {
                TransferError::Configuration(format!("reading {}: {error}", path.display()))
            })
        })
        .transpose()?;
    let proxy_var = |names: &[&str]| names.iter().find_map(|name| environment.string(name));
    Ok(crate::transport::http::HttpEnvironment {
        ssl_ca_info,
        ssl_no_verify: environment
            .bytes("GIT_SSL_NO_VERIFY")
            .map(|value| crate::config::boolean(Some(&value)).unwrap_or(true)),
        proxy: proxy_var(&[
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "all_proxy",
            "ALL_PROXY",
        ]),
        no_proxy: proxy_var(&["no_proxy", "NO_PROXY"]),
        proxy_credentials: None,
        allow_insecure_tls: true,
    })
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
fn ssh_endpoint(
    config: &crate::Config,
    destination: &Destination,
    environment: &Environment,
) -> Result<crate::transport::ssh::SshRemote, TransferError> {
    use crate::transport::ssh::{ApprovedSshCommand, OpenSshOptions, SshRemote};
    // Git runs a configured command through the shell with the SSH arguments appended.
    let configured = environment
        .bytes("GIT_SSH_COMMAND")
        .or_else(|| {
            config
                .string("core", None, "sshcommand")
                .map(<[u8]>::to_vec)
        })
        .map(|command| (command.clone(), command))
        .or_else(|| {
            environment.bytes("GIT_SSH").map(|program| {
                let quoted = shell_quote(&String::from_utf8_lossy(&program));
                (program, quoted.into_bytes())
            })
        });
    let approved_command = configured.map(|(configured, command)| {
        let command = String::from_utf8_lossy(&command).into_owned();
        ApprovedSshCommand {
            configured,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".into(), format!("{command} \"$@\""), command],
        }
    });
    let default_executable =
        find_program(environment, "ssh").unwrap_or_else(|| PathBuf::from("/usr/bin/ssh"));
    // GIT_SSH_VARIANT and ssh.variant only matter for non-OpenSSH clients.
    let environment = environment
        .clone()
        .without("GIT_PROTOCOL")
        .without("GIT_SSH_VARIANT");
    SshRemote::openssh(
        config,
        destination,
        OpenSshOptions {
            default_executable,
            approved_command,
            environment: environment.vars,
        },
    )
    .map_err(|error| TransferError::Configuration(error.to_string()))
}

impl Repository {
    fn endpoint(
        &self,
        remote_name: &str,
        direction: Direction,
        environment: &Environment,
    ) -> Result<(Endpoint, Destination), TransferError> {
        let config = self.config();
        let remote = Remote::find(config, remote_name.as_bytes())
            .map_err(|error| TransferError::Configuration(error.to_string()))?
            .ok_or_else(|| TransferError::NoSuchRemote(remote_name.to_owned()))?;
        let protocol_environment =
            ProtocolEnvironment::from_environment(|name| environment.bytes(name))
                .map_err(|error| TransferError::Configuration(error.to_string()))?;
        let destination = match direction {
            Direction::Fetch => remote.fetch_destination(config, &protocol_environment),
            Direction::Push => remote
                .push_destinations(config, &protocol_environment)
                .and_then(|destinations| {
                    destinations
                        .into_iter()
                        .next()
                        .ok_or(crate::remote::EndpointError::Missing)
                }),
        }
        .map_err(|error| match error {
            crate::remote::EndpointError::Missing => {
                TransferError::NoSuchRemote(remote_name.to_owned())
            }
            error => TransferError::Configuration(error.to_string()),
        })?;
        let endpoint = match destination.protocol() {
            Protocol::Local | Protocol::File => {
                let path = destination
                    .local_path()
                    .map_err(|error| TransferError::Configuration(error.to_string()))?;
                // Like Git, relative paths are relative to the working directory, falling back to
                // the working tree (or Git directory).
                let base = environment
                    .current_dir
                    .as_deref()
                    .or(self.worktree())
                    .unwrap_or(self.git_dir());
                Endpoint::Local(base.join(path))
            }
            #[cfg(feature = "http")]
            Protocol::Http | Protocol::Https => Endpoint::Http(Box::new(Http {
                environment: http_environment(config, &destination, environment)?,
                helpers: credential_helpers(config, environment),
                display_url: redacted_url(destination.bytes()),
                settings_config: config.clone(),
                remote_name: remote_name.as_bytes().to_vec(),
                destination: destination.clone(),
            })),
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Protocol::Ssh => {
                Endpoint::Ssh(Box::new(ssh_endpoint(config, &destination, environment)?))
            }
            _ => return Err(TransferError::UnsupportedTransport(remote_name.to_owned())),
        };
        Ok((endpoint, destination))
    }

    /// Asks the remote which branch its HEAD points to.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError`] for configuration, transport and authentication failures.
    pub fn remote_head(
        &self,
        remote_name: &str,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<RemoteHead, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Fetch, environment)?;
        let cancel = AtomicBool::new(false);
        let control = TransportControl::new(&cancel);
        let limits = FetchLimits::default();
        let discovery = match endpoint {
            Endpoint::Local(path) => crate::fetch::discover_local(path, limits, control),
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let result = runtime.block_on(crate::fetch::discover_http(
                    &http.remote(None)?,
                    limits,
                    control,
                ));
                if is_unauthorized_fetch(&result) {
                    let session = http.credentials(callbacks)?;
                    runtime.block_on(crate::fetch::discover_http(
                        &http.remote(Some(session))?,
                        limits,
                        control,
                    ))
                } else {
                    result
                }
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                runtime()?.block_on(crate::fetch::discover_ssh(&ssh, limits, control))
            }
        };
        let _ = &callbacks;
        discovery
            .map(|discovery| discovery.head)
            .map_err(|error| TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error))))
    }

    /// Fetches from a configured remote and updates local references.
    ///
    /// Destinations must be remote-tracking references (`refs/remotes/`), tags, or a namespace
    /// allowed with [`FetchOptions::tag_namespace`]. Exact sources the remote doesn't have are
    /// reported in [`FetchOutcome::missing_sources`] rather than failing the fetch. No `FETCH_HEAD`
    /// is written and tags are not followed implicitly.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError`]. After [`TransferError::FetchFinish`], new objects are installed
    /// but some references may not have been updated.
    pub fn fetch(
        &self,
        remote_name: &str,
        options: &FetchOptions,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<FetchOutcome, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Fetch, environment)?;
        let objects = self
            .objects(crate::PackLimits::default())
            .map_err(TransferError::local)?;
        let refs = self.references().map_err(TransferError::local)?;
        let tips: Vec<ObjectId> = refs
            .list()
            .map_err(TransferError::local)?
            .into_iter()
            .filter_map(|reference| match reference.target {
                crate::refs::Target::Direct(id) => Some(id),
                crate::refs::Target::Symbolic(_) => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let cancel = AtomicBool::new(false);
        let limits = fetch_limits();
        let known = KnownHistory::from_store(objects, &tips, limits, &cancel)
            .map_err(|error| TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error))))?;
        let request = || self.fetch_request(options);
        let control = TransportControl::new(&cancel);
        let mut on_message = |message: &[u8]| {
            callbacks.remote_message(message);
            ControlFlow::Continue(())
        };
        let ready = match endpoint {
            Endpoint::Local(path) => {
                request()?.receive_local(path, &known, limits, control, &mut on_message)
            }
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let known = std::sync::Arc::new(known);
                let receive = |remote: crate::transport::http::HttpRemote,
                               messages: &mut dyn FnMut(&[u8]) -> ControlFlow<()>|
                 -> Result<_, TransferError> {
                    let download = runtime.block_on(request()?.receive_http_with_progress(
                        &remote,
                        Some(known.clone()),
                        limits,
                        control,
                        |message| {
                            let _ = messages(message);
                        },
                    ));
                    Ok(download)
                };
                let mut download = receive(http.remote(None)?, &mut on_message)?;
                if matches!(
                    &download,
                    Err(FetchWorkflowError::Transfer(
                        crate::fetch::FetchError::Http(crate::transport::http::HttpError::Status(
                            401
                        ))
                    ))
                ) {
                    let session = http.credentials(callbacks)?;
                    download = receive(http.remote(Some(session))?, &mut |message| {
                        callbacks.remote_message(message);
                        ControlFlow::Continue(())
                    })?;
                }
                download
                    .and_then(|download| download.validate(&cancel, |_| ControlFlow::Continue(())))
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                let callbacks = std::cell::RefCell::new(&mut *callbacks);
                let download = runtime()?.block_on(request()?.receive_ssh_with_progress(
                    &ssh,
                    Some(std::sync::Arc::new(known)),
                    limits,
                    control,
                    |message| callbacks.borrow_mut().transport_message(message),
                    |message| callbacks.borrow_mut().remote_message(message),
                ));
                download
                    .and_then(|download| download.validate(&cancel, |_| ControlFlow::Continue(())))
            }
        }
        .map_err(|error| TransferError::Fetch(Box::new(error)))?;
        let report = ready
            .finish(FetchUpdateLimits::default(), &cancel)
            .map_err(|error| TransferError::FetchFinish(Box::new(error)))?;
        Ok(FetchOutcome {
            updated: report
                .updates
                .iter()
                .filter(|update| {
                    !matches!(
                        update.kind,
                        FetchUpdateKind::SourceOnly | FetchUpdateKind::Unchanged
                    )
                })
                .filter_map(|update| {
                    Some(FetchedRef {
                        name: update.mapping.destination.clone()?,
                        old: update.previous,
                        new: update.mapping.source.as_ref().map(|source| source.id),
                    })
                })
                .collect(),
            missing_sources: report.missing,
        })
    }

    fn fetch_request(&self, options: &FetchOptions) -> Result<FetchRequest, TransferError> {
        let specs = Refspecs::parse(Direction::Fetch, options.refspecs.iter())
            .map_err(|error| TransferError::Configuration(error.to_string()))?;
        // Forced refspecs may replace any existing destination, as with `git fetch`.
        let authorized: BTreeSet<RefName> = self
            .references()
            .map_err(TransferError::local)?
            .list()
            .map_err(TransferError::local)?
            .into_iter()
            .map(|reference| reference.name)
            .collect();
        let reflog = match &options.reflog {
            None => Reflog::Preserve,
            Some((committer, message)) => Reflog::AppendIfChanged {
                committer: committer.clone(),
                message: message.clone(),
            },
        };
        let mut request = FetchRequest::prepare(self.clone(), specs, authorized, reflog)
            .map_err(|error| TransferError::Fetch(Box::new(error.into())))?;
        if options.prune {
            // Pruned references lose their reflogs, as in Git.
            request = request.with_prune();
            let namespaces = std::iter::once(RefName::new("refs/remotes").expect("valid name"))
                .chain(options.tag_namespaces.iter().cloned());
            for namespace in namespaces {
                request = request.with_reflog_for_update_kind(
                    namespace,
                    FetchUpdateKind::Prune,
                    Reflog::Delete,
                );
            }
        }
        for namespace in &options.tag_namespaces {
            request = request.with_tag_destination_namespace(namespace.clone());
        }
        if let Some(depth) = options.depth {
            request = request.with_depth(depth);
        }
        Ok(request)
    }

    /// Updates references on a configured remote's push URL.
    ///
    /// Each command is a compare-and-swap on the remote: it only applies if the remote reference
    /// currently has the command's expected value. Commands succeed or fail independently; check
    /// each [`crate::push::RefStatus`] in the report.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Push`] when the push couldn't complete. Its
    /// [`crate::push::PushError::Uncertain`] variant means some updates may have been applied.
    pub fn push(
        &self,
        remote_name: &str,
        commands: Vec<PushCommand>,
        options: &PushOptions,
        environment: &Environment,
        callbacks: &mut dyn TransferCallbacks,
    ) -> Result<PushReport, TransferError> {
        let (endpoint, _) = self.endpoint(remote_name, Direction::Push, environment)?;
        let objects = self
            .objects(crate::PackLimits::default())
            .map_err(TransferError::local)?;
        let cancel = AtomicBool::new(false);
        let control = TransportControl::new(&cancel);
        // Objects the remote already has: the expected values of the commands, which the remote
        // must have for the push to succeed.
        let receiver_roots: Vec<ObjectId> = commands
            .iter()
            .filter_map(|command| command.expected)
            .collect();
        let prepare = |roots: &[ObjectId]| -> Result<PreparedPush, TransferError> {
            let prepared =
                PreparedPush::new_local(&objects, commands.clone(), roots, push_limits(), &cancel)
                    .map_err(|error| {
                        TransferError::Push(Box::new(crate::push::PushError::NotSent(error)))
                    })?;
            let prepared = if options.push_options.is_empty() {
                prepared
            } else {
                prepared
                    .with_push_options(options.push_options.clone())
                    .map_err(|error| {
                        TransferError::Push(Box::new(crate::push::PushError::NotSent(error)))
                    })?
            };
            Ok(prepared.with_progress())
        };
        let result = match endpoint {
            Endpoint::Local(path) => {
                let send = |roots: &[ObjectId]| -> Result<_, TransferError> {
                    let prepared = prepare(roots)?;
                    Ok(crate::push::send_local_with_context(
                        &path,
                        &prepared,
                        control,
                        crate::push::LocalPushContext {
                            config_inputs: &crate::config::ConfigInputs::default(),
                            identity: options.identity.as_ref(),
                        },
                    ))
                };
                let mut result = send(&receiver_roots)?;
                // An expected value the remote doesn't have can't be excluded from the pack.
                if is_knowledge_changed(&result) {
                    result = send(&[])?;
                }
                // Receive hook output arrives with the report rather than live.
                if let Ok(report) = &result {
                    for message in &report.progress {
                        callbacks.remote_message(message);
                    }
                }
                result
            }
            #[cfg(feature = "http")]
            Endpoint::Http(http) => {
                let runtime = runtime()?;
                let send = |remote: &crate::transport::http::HttpRemote,
                            roots: &[ObjectId],
                            callbacks: &mut dyn TransferCallbacks| {
                    let prepared = prepare(roots)?;
                    let names: Vec<RefName> =
                        prepared.commands().iter().map(|c| c.name.clone()).collect();
                    let advertised = std::cell::RefCell::new(BTreeMap::new());
                    let result = runtime.block_on(crate::push::send_http_checked_with_progress(
                        remote,
                        prepared,
                        control,
                        |advertisement| {
                            record_targets(advertisement, &names, &advertised);
                            true
                        },
                        |message| callbacks.remote_message(message),
                    ));
                    Ok::<_, TransferError>(result.map(|outcome| match outcome {
                        crate::push::HttpPushOutcome::Sent(mut report) => {
                            mark_up_to_date(&mut report, &advertised.into_inner());
                            crate::push::HttpPushOutcome::Sent(report)
                        }
                        outcome => outcome,
                    }))
                };
                let remote = http.remote(None)?;
                let mut result = send(&remote, &receiver_roots, callbacks)?;
                if is_unauthorized_push(&result) {
                    let session = http.credentials(callbacks)?;
                    let remote = http.remote(Some(session))?;
                    result = send(&remote, &receiver_roots, callbacks)?;
                }
                if is_knowledge_changed(&result) {
                    result = send(&remote, &[], callbacks)?;
                }
                result.map(|outcome| match outcome {
                    crate::push::HttpPushOutcome::Sent(report) => report,
                    crate::push::HttpPushOutcome::Declined => {
                        unreachable!("push is never declined")
                    }
                })
            }
            #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
            Endpoint::Ssh(ssh) => {
                let runtime = runtime()?;
                let send = |roots: &[ObjectId], callbacks: &mut dyn TransferCallbacks| {
                    let prepared = prepare(roots)?;
                    let names: Vec<RefName> =
                        prepared.commands().iter().map(|c| c.name.clone()).collect();
                    let advertised = std::cell::RefCell::new(BTreeMap::new());
                    let callbacks = std::cell::RefCell::new(callbacks);
                    let result = runtime.block_on(crate::push::send_ssh_checked_with_progress(
                        &ssh,
                        &prepared,
                        control,
                        |advertisement| {
                            record_targets(advertisement, &names, &advertised);
                            true
                        },
                        |message| callbacks.borrow_mut().transport_message(message),
                        |message| callbacks.borrow_mut().remote_message(message),
                    ));
                    Ok::<_, TransferError>(result.map(|outcome| match outcome {
                        crate::push::SshPushOutcome::Sent(mut report) => {
                            mark_up_to_date(&mut report, &advertised.into_inner());
                            crate::push::SshPushOutcome::Sent(report)
                        }
                        outcome => outcome,
                    }))
                };
                let mut result = send(&receiver_roots, callbacks)?;
                if is_knowledge_changed(&result) {
                    result = send(&[], callbacks)?;
                }
                result.map(|outcome| match outcome {
                    crate::push::SshPushOutcome::Sent(report) => report,
                    crate::push::SshPushOutcome::Declined => {
                        unreachable!("push is never declined")
                    }
                })
            }
        };
        let report = result.map_err(|error| TransferError::Push(Box::new(error)))?;
        self.update_tracking_refs(remote_name, &report, options.identity.as_ref());
        Ok(report)
    }

    /// Updates local remote-tracking references for successfully pushed references, as `git
    /// push` does, using the remote's fetch refspecs. Failures are ignored: the next fetch
    /// corrects them.
    fn update_tracking_refs(
        &self,
        remote_name: &str,
        report: &PushReport,
        identity: Option<&crate::Signature>,
    ) {
        let Ok(Some(remote)) =
            crate::remote::ConfiguredRemote::find(self.config(), remote_name.as_bytes())
        else {
            return;
        };
        let Ok(refs) = self.references() else {
            return;
        };
        let succeeded = report.unpack == Some(crate::push::Status::Ok);
        for status in &report.refs {
            if !succeeded || status.status != Some(crate::push::Status::Ok) {
                continue;
            }
            let Some(name) = tracking_ref(remote.fetch_refspecs(), status.command.name.as_bytes())
            else {
                continue;
            };
            let deleted = status.command.deletes();
            let edit = crate::refs::RefEdit {
                name,
                dereference: false,
                target: (!deleted).then_some(crate::refs::Target::Direct(status.command.new)),
                expected: crate::refs::Expected::Any,
                reflog: match (deleted, identity) {
                    (true, _) => Reflog::Delete,
                    (false, Some(identity)) => Reflog::AppendIfChanged {
                        committer: identity.clone(),
                        message: b"update by push".to_vec(),
                    },
                    (false, None) => Reflog::Preserve,
                },
            };
            // Applied independently so one conflicting name doesn't block the others.
            let _ = refs.transaction(std::slice::from_ref(&edit));
        }
    }
}

/// Maps a remote reference name through fetch refspecs to its remote-tracking reference.
fn tracking_ref(refspecs: &[crate::remote::ConfiguredRefspec], name: &[u8]) -> Option<RefName> {
    use crate::remote::ConfiguredRefspecKind;
    let matches = |pattern: &[u8]| -> Option<Vec<u8>> {
        match pattern.iter().position(|b| *b == b'*') {
            None => (pattern == name).then(Vec::new),
            Some(star) => {
                let (prefix, suffix) = (&pattern[..star], &pattern[star + 1..]);
                (name.len() >= prefix.len() + suffix.len()
                    && name.starts_with(prefix)
                    && name.ends_with(suffix))
                .then(|| name[prefix.len()..name.len() - suffix.len()].to_vec())
            }
        }
    };
    let excluded = refspecs.iter().any(|spec| {
        spec.kind() == ConfiguredRefspecKind::Exclusion && spec.source().and_then(matches).is_some()
    });
    if excluded {
        return None;
    }
    refspecs
        .iter()
        .filter(|spec| spec.kind() == ConfiguredRefspecKind::Mapping)
        .find_map(|spec| {
            let captured = matches(spec.source()?)?;
            let destination = spec.destination()?;
            let destination = match destination.iter().position(|b| *b == b'*') {
                None => destination.to_vec(),
                Some(star) => [&destination[..star], &captured, &destination[star + 1..]].concat(),
            };
            RefName::new(destination).ok()
        })
}

/// Records the advertised values of the pushed references.
#[cfg(any(feature = "http", feature = "ssh"))]
fn record_targets(
    advertisement: &crate::push::PushAdvertisement,
    names: &[RefName],
    advertised: &std::cell::RefCell<BTreeMap<RefName, Option<ObjectId>>>,
) {
    let mut advertised = advertised.borrow_mut();
    for name in names {
        advertised.insert(name.clone(), advertisement.target(name));
    }
}

/// Like Git, treats a rejected reference whose remote value already equals the requested value as
/// up to date.
#[cfg(any(feature = "http", feature = "ssh"))]
fn mark_up_to_date(report: &mut PushReport, advertised: &BTreeMap<RefName, Option<ObjectId>>) {
    for status in &mut report.refs {
        let Some(current) = advertised.get(&status.command.name) else {
            continue;
        };
        // Deletions are never up to date; see `git push --force-with-lease`.
        let up_to_date = !status.command.deletes() && *current == Some(status.command.new);
        if up_to_date && status.status != Some(crate::push::Status::Ok) {
            status.status = Some(crate::push::Status::Ok);
            status.rejection_origin = None;
        }
    }
}

fn is_knowledge_changed<T>(result: &Result<T, crate::push::PushError>) -> bool {
    matches!(
        result,
        Err(crate::push::PushError::NotSent(
            crate::push::PushFailure::KnowledgeChanged(_)
        ))
    )
}

#[cfg(feature = "http")]
fn is_unauthorized_push<T>(result: &Result<T, crate::push::PushError>) -> bool {
    matches!(
        result,
        Err(crate::push::PushError::NotSent(
            crate::push::PushFailure::Http(crate::transport::http::HttpError::Status(401))
        ))
    )
}

#[cfg(feature = "http")]
fn is_unauthorized_fetch<T>(result: &Result<T, crate::fetch::FetchError>) -> bool {
    matches!(
        result,
        Err(crate::fetch::FetchError::Http(
            crate::transport::http::HttpError::Status(401)
        ))
    )
}

#[cfg(any(feature = "http", feature = "ssh"))]
fn runtime() -> Result<tokio::runtime::Runtime, TransferError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(TransferError::local)
}

/// Resource limits sized for ordinary repositories rather than untrusted input.
fn fetch_limits() -> FetchLimits {
    FetchLimits {
        max_known_objects: usize::MAX / 4,
        max_known_bytes: usize::MAX / 4,
        max_known_edges: usize::MAX / 4,
        max_wants: usize::MAX / 4,
        ..FetchLimits::default()
    }
}

fn push_limits() -> PushLimits {
    PushLimits {
        max_edges: usize::MAX / 4,
        max_ancestry_steps: usize::MAX / 4,
        ..PushLimits::default()
    }
}
