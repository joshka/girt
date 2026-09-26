//! Explicit credential helper and askpass lifecycle.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::Config;
use crate::config::boolean;

/// A credential scope supplied by the application after URL parsing.
///
/// Components must not contain CR, LF, or NUL. A path is sent only when `use_http_path` is true.
#[derive(Clone)]
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

impl std::fmt::Debug for CredentialContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialContext").finish_non_exhaustive()
    }
}

/// Secret-bearing helper response. Debug deliberately omits all values.
#[derive(Clone, Default)]
pub struct Credential {
    /// Username returned by a helper or prompt.
    pub username: Option<Vec<u8>>,
    /// Password or token returned by a helper or prompt.
    pub password: Option<Vec<u8>>,
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
#[derive(Clone)]
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

impl std::fmt::Debug for CredentialProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialProgram").finish_non_exhaustive()
    }
}

/// A configured helper that the application explicitly mapped to a trusted program.
#[derive(Clone)]
pub struct CredentialHelper {
    /// Exact configuration value; used only to match the selected helper order.
    pub configured_name: Vec<u8>,
    /// Approved process invocation.
    pub program: CredentialProgram,
}

impl std::fmt::Debug for CredentialHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHelper").finish_non_exhaustive()
    }
}

/// Value-free error with a stage for recovery decisions.
#[derive(Debug, thiserror::Error)]
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

/// Ordered helper session. The application owns the allowlist, prompting, and any saved secret.
///
/// Select the helpers from config, map each name to a trusted program, then call `fill`. Keep the
/// returned session through authentication so `approve` or `reject` can notify the same helpers.
/// All methods are blocking; call them on an application worker when used from an async runtime.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::Config;
/// use girt::remote::{CredentialContext, CredentialSession, Prompt};
///
/// let config = Config::parse(b"").unwrap();
/// let context = CredentialContext {
///     protocol: b"https".to_vec(),
///     host: b"example.invalid".to_vec(),
///     path: Some(b"repo".to_vec()),
///     use_http_path: false,
/// };
/// let cancel = AtomicBool::new(false);
/// let mut prompt = |field| {
///     Some(match field {
///         Prompt::Username => b"sample-user".to_vec(),
///         Prompt::Password => b"synthetic-secret".to_vec(),
///     })
/// };
/// let session =
///     CredentialSession::fill(&config, context, &[], Some(&mut prompt), &cancel, None).unwrap();
/// assert!(session.credential().complete());
/// ```
pub struct CredentialSession {
    context: CredentialContext,
    helpers: Vec<CredentialProgram>,
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
            helpers.push(selected.program.clone());
        }
        let mut credential = Credential {
            username,
            password: None,
        };
        let mut helper_failures = Vec::new();
        for (position, helper) in helpers.iter().enumerate() {
            let input = encode(&context, &credential)?;
            let output = match run(helper, Some("get"), &input, cancel, deadline) {
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

    /// Notify all selected helpers after successful authentication.
    pub fn approve(
        &self,
        cancel: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Result<(), CredentialError> {
        self.notify("store", cancel, deadline)
    }

    /// Notify all selected helpers after rejected authentication.
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
            run(helper, Some(action), &input, cancel, deadline)?;
        }
        Ok(())
    }
}

/// A field requested by the application-owned prompt callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Username,
    Password,
}

/// Run an explicitly approved askpass executable for one prompt; `None` means user cancellation.
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
    written.map_err(|source| CredentialError::Io {
        stage: "stdin",
        source,
    })?;
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
            program: CredentialProgram {
                executable: PathBuf::from("sh"),
                arguments: vec![path.into_os_string()],
                inherit_environment: false,
                environment: Vec::new(),
            },
        };
        (directory, helper)
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
            askpass(&program.program, "Password:", &cancel, None).unwrap(),
            Some(b"synthetic-secret".to_vec())
        );
        let (_directory, empty) = helper("printf '\\n'");
        assert_eq!(
            askpass(&empty.program, "Password:", &cancel, None).unwrap(),
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
            program: CredentialProgram {
                executable: PathBuf::from("synthetic-secret"),
                arguments: vec!["synthetic-secret".into()],
                inherit_environment: false,
                environment: vec![("TOKEN".into(), "synthetic-secret".into())],
            },
        };
        assert!(!format!("{helper:?}").contains("synthetic-secret"));
        assert!(!format!("{:?}", helper.program).contains("synthetic-secret"));
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
            program: CredentialProgram {
                executable: PathBuf::from("cmd.exe"),
                arguments: vec!["/D".into(), "/C".into(), path.into_os_string()],
                inherit_environment: false,
                environment: Vec::new(),
            },
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
