//! SSH signatures made and checked with `ssh-keygen -Y`.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{Error, Programs, SignatureFormat, SignedPayload, Signer, Status, Trust, Verification};
use crate::Config;
use crate::workflow::Environment;

/// The namespace Git signs and verifies under, so a commit signature cannot pass as a file one.
const NAMESPACE: &str = "git";

/// The key an [`SshSigner`] signs with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SshKey {
    /// A private key file, or a public key file whose private key sits beside it or in the agent.
    Path(PathBuf),
    /// A public key given inline, as `user.signingKey = key::ssh-ed25519 AAAA…` configures. The
    /// private key must be in the SSH agent.
    Literal(String),
}

/// Signs payloads with `ssh-keygen -Y sign`, as Git does when `gpg.format` is `ssh`.
///
/// Signing runs `gpg.ssh.program` (default `ssh-keygen`) only under [`Programs::Run`]; under
/// [`Programs::Refuse`] [`Signer::sign`] returns [`Error::Refused`]. A [`SshKey::Literal`] key is
/// written to a temporary file and signed through the SSH agent named by `SSH_AUTH_SOCK`. The
/// program inherits this process's environment unless [`Self::with_environment`] replaces it.
///
/// ```no_run
/// use girt::signing::{Programs, Signer, SshKey, SshSigner};
///
/// let signer = SshSigner::new(SshKey::Path("/home/me/.ssh/id_ed25519".into()))
///     .with_programs(Programs::Run);
/// let signature = signer.sign(b"payload")?;
/// assert!(signature.starts_with(b"-----BEGIN SSH SIGNATURE-----\n"));
/// # Ok::<(), girt::signing::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct SshSigner {
    key: SshKey,
    program: OsString,
    programs: Programs,
    environment: Option<Environment>,
}

impl SshSigner {
    /// Signs with `key` through `ssh-keygen`, refusing to run it until
    /// [`Self::with_programs`] allows it.
    pub fn new(key: SshKey) -> Self {
        Self {
            key,
            program: "ssh-keygen".into(),
            programs: Programs::Refuse,
            environment: None,
        }
    }

    /// Reads `user.signingKey`, `gpg.ssh.defaultKeyCommand` and `gpg.ssh.program`.
    ///
    /// A signing key starting with `key::` or `ssh-` is a literal public key; anything else is a
    /// path, with a leading `~/` expanded from `HOME` in `environment`. Without a signing key, the
    /// first line of `gpg.ssh.defaultKeyCommand`'s output must be a literal key; the command runs
    /// through `sh -c` with exactly `environment`, only under [`Programs::Run`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSigningKey`] when neither setting yields a key, [`Error::Refused`] when
    /// the default key command would run under [`Programs::Refuse`], and the command's failure.
    pub fn from_config(
        config: &Config,
        environment: &Environment,
        programs: Programs,
    ) -> Result<Self, Error> {
        let key = match config.string("user", None, "signingkey") {
            Some(value) if !value.is_empty() => parse_key(value, environment)?,
            _ => {
                let command = config
                    .string("gpg", Some(b"ssh"), "defaultkeycommand")
                    .filter(|command| !command.is_empty())
                    .ok_or(Error::NoSigningKey)?;
                default_key(command, programs, environment)?
            }
        };
        let mut signer = Self::new(key).with_programs(programs);
        if let Some(program) = program(config)? {
            signer.program = program;
        }
        Ok(signer)
    }

    /// Replaces whether `ssh-keygen` may run.
    #[must_use]
    pub fn with_programs(mut self, programs: Programs) -> Self {
        self.programs = programs;
        self
    }

    /// Replaces the program run in place of `ssh-keygen`, as `gpg.ssh.program` does.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    /// Runs `ssh-keygen` with exactly `environment` instead of inheriting this process's.
    ///
    /// `SSH_AUTH_SOCK` in the environment names the agent `ssh-keygen` may contact; leave it out
    /// to keep the agent out of reach.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = Some(environment);
        self
    }

    /// The key this signer uses.
    pub fn key(&self) -> &SshKey {
        &self.key
    }
}

impl Signer for SshSigner {
    fn format(&self) -> SignatureFormat {
        SignatureFormat::Ssh
    }

    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        self.programs.check(&self.program)?;
        let literal;
        let mut args: Vec<OsString> =
            vec!["-Y".into(), "sign".into(), "-n".into(), NAMESPACE.into()];
        match &self.key {
            SshKey::Path(path) => args.extend(["-f".into(), path.into()]),
            SshKey::Literal(key) => {
                literal = temporary(key.as_bytes())?;
                args.extend(["-f".into(), literal.path().into(), "-U".into()]);
            }
        }
        let output = run(&self.program, &args, payload, self.environment.as_ref())?;
        if !output.status.success() {
            return Err(Error::failed(&self.program, &output));
        }
        Ok(output.stdout)
    }
}

/// Checks SSH signatures with `ssh-keygen -Y`, as Git does when the signature is SSH armor.
///
/// Verification asks `ssh-keygen -Y find-principals` which principal in the allowed signers file
/// owns the key, then runs `ssh-keygen -Y verify` for the first one, also passing the revocation
/// file. A signature whose key no principal owns is still checked with
/// `ssh-keygen -Y check-novalidate` and reported as [`Status::UnknownValidity`]. When a signing
/// time is given it is passed as `-Overify-time`, so `valid-after` and `valid-before` apply at
/// that time; Git passes the committer or tagger date.
///
/// Programs run only under [`Programs::Run`].
#[derive(Clone, Debug)]
pub struct SshVerifier {
    allowed_signers: Option<PathBuf>,
    revocations: Option<PathBuf>,
    program: OsString,
    programs: Programs,
    environment: Option<Environment>,
}

impl SshVerifier {
    /// Checks against an allowed signers file, refusing to run `ssh-keygen` until
    /// [`Self::with_programs`] allows it.
    pub fn new(allowed_signers: impl Into<PathBuf>) -> Self {
        Self {
            allowed_signers: Some(allowed_signers.into()),
            revocations: None,
            program: "ssh-keygen".into(),
            programs: Programs::Refuse,
            environment: None,
        }
    }

    /// Reads `gpg.ssh.allowedSignersFile`, `gpg.ssh.revocationFile` and `gpg.ssh.program`.
    ///
    /// A leading `~/` in a path is expanded from `HOME` in `environment`. A missing allowed signers
    /// file is not an error here; [`Self::verify`] reports it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] for a path that cannot be expanded.
    pub fn from_config(
        config: &Config,
        environment: &Environment,
        programs: Programs,
    ) -> Result<Self, Error> {
        let path = |name| {
            config
                .string("gpg", Some(b"ssh"), name)
                .map(|value| expand(value, environment))
                .transpose()
        };
        Ok(Self {
            allowed_signers: path("allowedsignersfile")?,
            revocations: path("revocationfile")?.filter(|path| !path.as_os_str().is_empty()),
            program: program(config)?.unwrap_or_else(|| "ssh-keygen".into()),
            programs,
            environment: None,
        })
    }

    /// Also rejects keys listed in `revocations`, as `gpg.ssh.revocationFile` does.
    #[must_use]
    pub fn with_revocations(mut self, revocations: impl Into<PathBuf>) -> Self {
        self.revocations = Some(revocations.into());
        self
    }

    /// Replaces whether `ssh-keygen` may run.
    #[must_use]
    pub fn with_programs(mut self, programs: Programs) -> Self {
        self.programs = programs;
        self
    }

    /// Replaces the program run in place of `ssh-keygen`, as `gpg.ssh.program` does.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    /// Runs `ssh-keygen` with exactly `environment` instead of inheriting this process's.
    ///
    /// `SSH_AUTH_SOCK` in the environment names the agent `ssh-keygen` may contact; leave it out
    /// to keep the agent out of reach.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Checks `signed`, applying validity periods at [`SignedPayload::signed_at`] when known.
    ///
    /// A signature that does not verify is a [`Status::Bad`] verification, not an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotSsh`] for a signature that is not SSH armor,
    /// [`Error::NoAllowedSigners`] when no allowed signers file is configured,
    /// [`Error::Refused`] under [`Programs::Refuse`], and failures to start `ssh-keygen` or write
    /// its temporary signature file.
    pub fn verify(&self, signed: &SignedPayload) -> Result<Verification, Error> {
        if signed.format() != Some(SignatureFormat::Ssh) {
            return Err(Error::NotSsh);
        }
        let allowed = self
            .allowed_signers
            .as_ref()
            .ok_or(Error::NoAllowedSigners)?;
        self.programs.check(&self.program)?;
        let signature = temporary(signed.signature())?;
        let mut options: Vec<OsString> = vec!["-s".into(), signature.path().into()];
        if let Some(time) = signed.signed_at().and_then(verify_time) {
            options.push(format!("-Overify-time={time}").into());
        }

        let mut find: Vec<OsString> = vec!["-Y".into(), "find-principals".into()];
        find.extend(["-f".into(), allowed.into()]);
        find.extend(options.iter().cloned());
        let found = run(&self.program, &find, b"", self.environment.as_ref())?;
        let principal = found
            .status
            .success()
            .then(|| found.stdout.split(|&byte| byte == b'\n').next())
            .flatten()
            .filter(|line| !line.is_empty());

        let mut args: Vec<OsString> = vec!["-Y".into()];
        match principal {
            Some(principal) => {
                args.extend(["verify".into(), "-n".into(), NAMESPACE.into()]);
                args.extend(["-f".into(), allowed.into(), "-I".into()]);
                args.push(String::from_utf8_lossy(principal).into_owned().into());
                args.extend(options);
                if let Some(revocations) = &self.revocations {
                    args.extend(["-r".into(), revocations.into()]);
                }
            }
            None => {
                args.extend(["check-novalidate".into(), "-n".into(), NAMESPACE.into()]);
                args.extend(options);
            }
        }
        let output = run(
            &self.program,
            &args,
            signed.payload(),
            self.environment.as_ref(),
        )?;
        let mut report = output.stdout.clone();
        report.extend_from_slice(&found.stderr);
        report.extend_from_slice(&output.stderr);
        Ok(interpret(
            output.status.success(),
            principal.is_some(),
            &output.stdout,
            report,
        ))
    }
}

/// Turns `ssh-keygen -Y verify` or `check-novalidate` output into a verification.
///
/// A good line reads `Good "git" signature for <principal> with <type> key <fingerprint>`, or
/// without `for <principal>` from `check-novalidate`.
fn interpret(success: bool, validated: bool, stdout: &[u8], output: Vec<u8>) -> Verification {
    if !success {
        return Verification {
            status: Status::Bad,
            signer: None,
            fingerprint: None,
            trust: Trust::Never,
            output,
        };
    }
    let line = String::from_utf8_lossy(
        stdout
            .split(|&byte| byte == b'\n')
            .find(|line| line.starts_with(b"Good \""))
            .unwrap_or_default(),
    )
    .into_owned();
    let fingerprint = line
        .rsplit_once(" key ")
        .map(|(_, fingerprint)| fingerprint.to_owned());
    let signer = line
        .split_once(" signature for ")
        .and_then(|(_, rest)| rest.rsplit_once(" with "))
        .map(|(principal, _)| principal.to_owned());
    let (status, trust) = if validated {
        (Status::Good, Trust::Fully)
    } else {
        (Status::UnknownValidity, Trust::Undefined)
    };
    Verification {
        status,
        signer: signer.filter(|_| validated),
        fingerprint,
        trust,
        output,
    }
}

/// `ssh-keygen`'s `-Overify-time` value for Unix seconds, in UTC so the host time zone is moot.
fn verify_time(seconds: i64) -> Option<String> {
    let time = jiff::Timestamp::from_second(seconds).ok()?;
    Some(time.strftime("%Y%m%d%H%M%SZ").to_string())
}

fn parse_key(value: &[u8], environment: &Environment) -> Result<SshKey, Error> {
    let text = std::str::from_utf8(value).map_err(|_| Error::Path("user.signingKey"))?;
    if let Some(key) = text.strip_prefix("key::") {
        return Ok(SshKey::Literal(key.to_owned()));
    }
    if text.starts_with("ssh-") {
        return Ok(SshKey::Literal(text.to_owned()));
    }
    Ok(SshKey::Path(expand(value, environment)?))
}

/// Runs `gpg.ssh.defaultKeyCommand` and takes a literal key from its first line.
fn default_key(
    command: &[u8],
    programs: Programs,
    environment: &Environment,
) -> Result<SshKey, Error> {
    let command = String::from_utf8_lossy(command).into_owned();
    programs.check(command.as_ref())?;
    let output = run(
        "sh".as_ref(),
        &["-c".into(), command.clone().into()],
        b"",
        Some(environment),
    )?;
    if !output.status.success() {
        return Err(Error::failed(command.as_ref(), &output));
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    match first.strip_prefix("key::") {
        Some(key) => Ok(SshKey::Literal(key.to_owned())),
        None if first.starts_with("ssh-") => Ok(SshKey::Literal(first)),
        None => Err(Error::NoSigningKey),
    }
}

fn program(config: &Config) -> Result<Option<OsString>, Error> {
    config
        .string("gpg", Some(b"ssh"), "program")
        .filter(|value| !value.is_empty())
        .map(|value| {
            crate::source_files::os_path(value)
                .map(PathBuf::into_os_string)
                .ok_or(Error::Path("gpg.ssh.program"))
        })
        .transpose()
}

fn expand(value: &[u8], environment: &Environment) -> Result<PathBuf, Error> {
    let path = |bytes| crate::source_files::os_path(bytes).ok_or(Error::Path("ssh signing path"));
    match value.strip_prefix(b"~/") {
        Some(rest) => {
            let home = environment
                .get("HOME")
                .ok_or(Error::Path("~/ without HOME"))?;
            Ok(Path::new(home).join(path(rest)?))
        }
        None => path(value),
    }
}

fn temporary(contents: &[u8]) -> Result<tempfile::NamedTempFile, Error> {
    let mut file = tempfile::NamedTempFile::new().map_err(Error::Temporary)?;
    file.write_all(contents).map_err(Error::Temporary)?;
    file.flush().map_err(Error::Temporary)?;
    Ok(file)
}

fn run(
    program: &std::ffi::OsStr,
    args: &[OsString],
    input: &[u8],
    environment: Option<&Environment>,
) -> Result<Output, Error> {
    let spawn = |source| Error::Spawn {
        program: program.to_owned(),
        source,
    };
    let mut command = Command::new(program);
    if let Some(environment) = environment {
        command.env_clear().envs(environment.vars());
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn)?;
    let stdin = child.stdin.take();
    // Write while the output is drained, so a program that answers before reading all of its
    // input cannot fill a pipe and wait on us.
    let (written, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.map(|mut stdin| stdin.write_all(input)));
        let output = child.wait_with_output();
        (writer.join(), output)
    });
    let output = output.map_err(spawn)?;
    // A program that exits without reading its input reports through its status instead.
    match written {
        Ok(Some(Err(error))) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(spawn(error)),
        Ok(_) => Ok(output),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn large_input_and_output_do_not_deadlock() {
        let input = vec![b'x'; 1 << 20];
        let args = [
            "-c".into(),
            "head -c 1048576 /dev/zero; cat >/dev/null".into(),
        ];
        let output = run("sh".as_ref(), &args, &input, None).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1 << 20);
    }

    #[test]
    fn verify_time_is_utc() {
        assert_eq!(verify_time(1_700_000_000).unwrap(), "20231114221320Z");
    }

    #[test]
    fn good_lines_give_signer_and_fingerprint() {
        let stdout = b"Good \"git\" signature for a@x with ED25519 key SHA256:abc\n";
        let verification = interpret(true, true, stdout, Vec::new());
        assert_eq!(verification.status, Status::Good);
        assert_eq!(verification.signer.as_deref(), Some("a@x"));
        assert_eq!(verification.fingerprint.as_deref(), Some("SHA256:abc"));

        let stdout = b"Good \"git\" signature with ED25519 key SHA256:abc\n";
        let verification = interpret(true, false, stdout, Vec::new());
        assert_eq!(verification.status, Status::UnknownValidity);
        assert_eq!(verification.signer, None);
        assert_eq!(verification.fingerprint.as_deref(), Some("SHA256:abc"));
    }

    #[test]
    fn signing_keys_are_literal_or_paths() {
        let environment = Environment::default().with("HOME", "/home/me");
        assert_eq!(
            parse_key(b"key::ssh-ed25519 AAAA", &environment).unwrap(),
            SshKey::Literal("ssh-ed25519 AAAA".into())
        );
        assert_eq!(
            parse_key(b"ssh-ed25519 AAAA", &environment).unwrap(),
            SshKey::Literal("ssh-ed25519 AAAA".into())
        );
        assert_eq!(
            parse_key(b"~/.ssh/id", &environment).unwrap(),
            SshKey::Path("/home/me/.ssh/id".into())
        );
    }
}
