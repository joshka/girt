//! OpenPGP and X.509 signatures made and checked with `gpg` and `gpgsm`.

use std::ffi::OsString;

use super::ssh::{run, temporary};
use super::{Error, Programs, SignatureFormat, SignedPayload, Signer, Status, Trust, Verification};
use crate::workflow::Environment;
use crate::{Config, Signature};

/// Signs payloads with `gpg` (OpenPGP) or `gpgsm` (X.509), as Git does when `gpg.format` is
/// `openpgp` or `x509`.
///
/// Signing runs `gpg --status-fd=2 -bsau <key>` (or `gpgsm` with the same arguments) with the
/// payload on standard input, and requires the program to report `SIG_CREATED`. Programs run only
/// under [`Programs::Run`], and inherit this process's environment, including `GNUPGHOME` and the
/// agent it names, unless [`Self::with_environment`] replaces it.
///
/// ```no_run
/// use girt::signing::{GpgSigner, Programs, SignatureFormat, Signer};
///
/// let signer =
///     GpgSigner::new(SignatureFormat::OpenPgp, "me@example.com")?.with_programs(Programs::Run);
/// let signature = signer.sign(b"payload")?;
/// assert!(signature.starts_with(b"-----BEGIN PGP SIGNATURE-----\n"));
/// # Ok::<(), girt::signing::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct GpgSigner {
    format: SignatureFormat,
    key: String,
    program: OsString,
    programs: Programs,
    environment: Option<Environment>,
}

impl GpgSigner {
    /// Signs in `format` with `key`, a key ID, fingerprint or user ID that `gpg -u` accepts,
    /// refusing to run the program until [`Self::with_programs`] allows it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongFormat`] when `format` is [`SignatureFormat::Ssh`]; use
    /// [`super::SshSigner`] for SSH keys.
    pub fn new(format: SignatureFormat, key: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            program: default_program(format)?,
            format,
            key: key.into(),
            programs: Programs::Refuse,
            environment: None,
        })
    }

    /// Reads `user.signingKey` and the program for `format` (`gpg.program` or
    /// `gpg.openpgp.program` for OpenPGP, whichever is set last; `gpg.x509.program` for X.509).
    ///
    /// Without a signing key, signs as the committer `Name <email>`, as Git does, using
    /// [`Signature::committer`] with `environment`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongFormat`] for SSH, [`Error::Identity`] when no committer identity is
    /// configured and no signing key is set, and [`Error::Path`] for an unusable program path.
    pub fn from_config(
        config: &Config,
        environment: &Environment,
        format: SignatureFormat,
        programs: Programs,
    ) -> Result<Self, Error> {
        let key = match config.string("user", None, "signingkey") {
            Some(key) if !key.is_empty() => String::from_utf8_lossy(key).into_owned(),
            _ => {
                let committer =
                    Signature::committer(config, environment).map_err(Error::Identity)?;
                format!(
                    "{} <{}>",
                    String::from_utf8_lossy(&committer.name),
                    String::from_utf8_lossy(&committer.email)
                )
            }
        };
        let mut signer = Self::new(format, key)?.with_programs(programs);
        if let Some(program) = configured_program(config, format)? {
            signer.program = program;
        }
        Ok(signer)
    }

    /// Replaces whether the program may run.
    #[must_use]
    pub fn with_programs(mut self, programs: Programs) -> Self {
        self.programs = programs;
        self
    }

    /// Replaces the program run in place of `gpg` or `gpgsm`.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    /// Runs the program with exactly `environment` instead of inheriting this process's.
    ///
    /// Set `GNUPGHOME` in it to choose the keyring.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = Some(environment);
        self
    }

    /// The key passed to `-u`.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Signer for GpgSigner {
    fn format(&self) -> SignatureFormat {
        self.format
    }

    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        self.programs.check(&self.program)?;
        let args = [
            "--status-fd=2".into(),
            "-bsau".into(),
            self.key.clone().into(),
        ];
        let output = run(&self.program, &args, payload, self.environment.as_ref())?;
        let created = output
            .stderr
            .split(|&byte| byte == b'\n')
            .any(|line| line.starts_with(b"[GNUPG:] SIG_CREATED "));
        if !output.status.success() || !created || output.stdout.is_empty() {
            return Err(Error::failed(&self.program, &output));
        }
        Ok(output.stdout)
    }
}

/// Checks OpenPGP signatures with `gpg` and X.509 signatures with `gpgsm`, as Git does.
///
/// Verification runs `gpg --keyid-format=long --status-fd=1 --verify <signature> -` (`gpgsm`
/// without `--keyid-format`) with the payload on standard input, and reads the status lines.
/// The keyring is whatever `GNUPGHOME` in the program's environment names. Programs run only under
/// [`Programs::Run`].
#[derive(Clone, Debug)]
pub struct GpgVerifier {
    openpgp: OsString,
    x509: OsString,
    min_trust: Trust,
    programs: Programs,
    environment: Option<Environment>,
}

impl Default for GpgVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl GpgVerifier {
    /// Checks with `gpg` and `gpgsm`, refusing to run them until [`Self::with_programs`] allows
    /// it.
    pub fn new() -> Self {
        Self {
            openpgp: "gpg".into(),
            x509: "gpgsm".into(),
            min_trust: Trust::Undefined,
            programs: Programs::Refuse,
            environment: None,
        }
    }

    /// Reads `gpg.program`, `gpg.openpgp.program`, `gpg.x509.program` and `gpg.minTrustLevel`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] for an unknown `gpg.minTrustLevel` and [`Error::Path`] for an
    /// unusable program path.
    pub fn from_config(config: &Config, programs: Programs) -> Result<Self, Error> {
        let mut verifier = Self::new().with_programs(programs);
        if let Some(program) = configured_program(config, SignatureFormat::OpenPgp)? {
            verifier.openpgp = program;
        }
        if let Some(program) = configured_program(config, SignatureFormat::X509)? {
            verifier.x509 = program;
        }
        if let Some(min_trust) = super::min_trust(config)? {
            verifier.min_trust = min_trust;
        }
        Ok(verifier)
    }

    /// Requires at least `min_trust` for [`Verification::is_good`], as `gpg.minTrustLevel` does.
    #[must_use]
    pub fn with_min_trust(mut self, min_trust: Trust) -> Self {
        self.min_trust = min_trust;
        self
    }

    /// Replaces whether programs may run.
    #[must_use]
    pub fn with_programs(mut self, programs: Programs) -> Self {
        self.programs = programs;
        self
    }

    /// Replaces the program for `format`, as `gpg.program` and `gpg.x509.program` do.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongFormat`] for SSH.
    pub fn with_program(
        mut self,
        format: SignatureFormat,
        program: impl Into<OsString>,
    ) -> Result<Self, Error> {
        match format {
            SignatureFormat::OpenPgp => self.openpgp = program.into(),
            SignatureFormat::X509 => self.x509 = program.into(),
            SignatureFormat::Ssh => return Err(Error::WrongFormat { format }),
        }
        Ok(self)
    }

    /// Runs programs with exactly `environment` instead of inheriting this process's.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Checks an OpenPGP or X.509 signature.
    ///
    /// A signature that does not verify is a [`Status::Bad`] verification and a missing key is
    /// [`Status::CannotCheck`] when the verifier emits `ERRSIG`. Without a verdict (for
    /// example, gpgsm reporting only `NO_PUBKEY`), the status is [`Status::NoSignature`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongFormat`] for an SSH or unrecognized signature, [`Error::Refused`]
    /// under [`Programs::Refuse`], and failures to start the program or write its temporary
    /// signature file.
    pub fn verify(&self, signed: &SignedPayload) -> Result<Verification, Error> {
        let (program, mut args): (&OsString, Vec<OsString>) = match signed.format() {
            Some(SignatureFormat::OpenPgp) => (&self.openpgp, vec!["--keyid-format=long".into()]),
            Some(SignatureFormat::X509) => (&self.x509, Vec::new()),
            Some(format @ SignatureFormat::Ssh) => return Err(Error::WrongFormat { format }),
            None => return Err(Error::UnknownFormat),
        };
        self.programs.check(program)?;
        let signature = temporary(signed.signature())?;
        args.extend([
            "--status-fd=1".into(),
            "--verify".into(),
            signature.path().into(),
            "-".into(),
        ]);
        let output = run(program, &args, signed.payload(), self.environment.as_ref())?;
        Ok(interpret(
            &output.stdout,
            output.status.success(),
            self.min_trust,
            {
                let mut report = output.stdout.clone();
                report.extend_from_slice(&output.stderr);
                report
            },
        ))
    }
}

/// Turns `[GNUPG:]` status lines into a verification.
/// `succeeded` is whether the program exited successfully; Git rejects a signature otherwise,
/// whatever the status lines say.
fn interpret(status: &[u8], succeeded: bool, min_trust: Trust, output: Vec<u8>) -> Verification {
    let text = String::from_utf8_lossy(status);
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("[GNUPG:] "))
        .collect();
    let mut verification = Verification {
        status: Status::NoSignature,
        signer: None,
        key: None,
        fingerprint: None,
        primary_fingerprint: None,
        trust: Trust::Undefined,
        output,
        accepted: false,
    };
    let mut verdict = None;
    let mut signatures = 0;
    for line in &lines {
        let (keyword, rest) = line.split_once(' ').unwrap_or((line, ""));
        let found = match keyword {
            "GOODSIG" => Some(Status::Good),
            "BADSIG" => Some(Status::Bad),
            "EXPSIG" => Some(Status::ExpiredSignature),
            "EXPKEYSIG" => Some(Status::ExpiredKey),
            "REVKEYSIG" => Some(Status::RevokedKey),
            "ERRSIG" => Some(Status::CannotCheck),
            _ => None,
        };
        if let Some(found) = found {
            let (key, signer) = rest.split_once(' ').unwrap_or((rest, ""));
            verification.key = Some(key.to_owned());
            verification.signer =
                (found != Status::CannotCheck && !signer.is_empty()).then(|| signer.to_owned());
            verdict = Some(found);
            signatures += 1;
            continue;
        }
        if keyword == "VALIDSIG" {
            let fields: Vec<&str> = rest.split(' ').collect();
            verification.fingerprint = fields.first().map(|&field| field.to_owned());
            verification.primary_fingerprint = fields.get(9).map(|&field| field.to_owned());
        } else if let Some(level) = keyword.strip_prefix("TRUST_") {
            verification.trust = match level {
                "NEVER" => Trust::Never,
                "MARGINAL" => Trust::Marginal,
                "FULLY" => Trust::Fully,
                "ULTIMATE" => Trust::Ultimate,
                _ => Trust::Undefined,
            };
        }
    }
    let Some(mut status) = verdict else {
        return verification;
    };
    if signatures > 1 {
        // Git checks exactly one signature; several, even all good, cannot be checked.
        verification.status = Status::CannotCheck;
        verification.signer = None;
        verification.key = None;
        verification.fingerprint = None;
        verification.primary_fingerprint = None;
        return verification;
    }
    if status == Status::Good && !verification.trust.at_least(Trust::Marginal) {
        status = Status::UnknownValidity;
    }
    verification.status = status;
    verification.accepted = succeeded
        && matches!(
            status,
            Status::Good | Status::UnknownValidity | Status::ExpiredKey
        )
        && verification.trust.at_least(min_trust);
    verification
}

fn default_program(format: SignatureFormat) -> Result<OsString, Error> {
    match format {
        SignatureFormat::OpenPgp => Ok("gpg".into()),
        SignatureFormat::X509 => Ok("gpgsm".into()),
        SignatureFormat::Ssh => Err(Error::WrongFormat { format }),
    }
}

/// The program configured for `format`; for OpenPGP the later of `gpg.program` and
/// `gpg.openpgp.program`.
fn configured_program(config: &Config, format: SignatureFormat) -> Result<Option<OsString>, Error> {
    let subsection: Option<&[u8]> = match format {
        SignatureFormat::OpenPgp => Some(b"openpgp"),
        SignatureFormat::X509 => Some(b"x509"),
        SignatureFormat::Ssh => return Err(Error::WrongFormat { format }),
    };
    let value = config
        .entries()
        .iter()
        .rev()
        .find(|entry| {
            entry.section.eq_ignore_ascii_case(b"gpg")
                && entry.name.eq_ignore_ascii_case(b"program")
                && (entry.subsection.as_deref() == subsection
                    || (format == SignatureFormat::OpenPgp && entry.subsection.is_none()))
        })
        .and_then(|entry| entry.value.as_deref())
        .filter(|value| !value.is_empty());
    value
        .map(|value| {
            crate::source_files::os_path(value)
                .map(std::path::PathBuf::into_os_string)
                .ok_or(Error::Path("gpg program"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "[GNUPG:] NEWSIG\n\
[GNUPG:] GOODSIG 36F2AA1E2C4A7E1B A <a@x>\n\
[GNUPG:] VALIDSIG 77D6 2026-10-18 1792313552 0 4 0 22 10 00 PRIMARY\n";

    #[test]
    fn trust_decides_good_or_unknown_validity() {
        for (line, status, trust, accepted) in [
            ("", Status::UnknownValidity, Trust::Undefined, true),
            (
                "[GNUPG:] TRUST_NEVER 0 pgp\n",
                Status::UnknownValidity,
                Trust::Never,
                false,
            ),
            (
                "[GNUPG:] TRUST_MARGINAL 0 pgp\n",
                Status::Good,
                Trust::Marginal,
                true,
            ),
            (
                "[GNUPG:] TRUST_ULTIMATE 0 pgp\n",
                Status::Good,
                Trust::Ultimate,
                true,
            ),
        ] {
            let lines = format!("{GOOD}{line}");
            let verification = interpret(lines.as_bytes(), true, Trust::Undefined, Vec::new());
            assert_eq!(verification.status, status);
            assert_eq!(verification.trust, trust);
            assert_eq!(verification.is_good(), accepted);
            assert_eq!(verification.signer.as_deref(), Some("A <a@x>"));
            assert_eq!(verification.key.as_deref(), Some("36F2AA1E2C4A7E1B"));
            assert_eq!(verification.fingerprint.as_deref(), Some("77D6"));
            assert_eq!(verification.primary_fingerprint.as_deref(), Some("PRIMARY"));
        }
    }

    #[test]
    fn min_trust_rejects_lower_trust() {
        let status = format!("{GOOD}[GNUPG:] TRUST_FULLY 0 pgp\n");
        assert!(interpret(status.as_bytes(), true, Trust::Fully, Vec::new()).is_good());
        assert!(!interpret(status.as_bytes(), true, Trust::Ultimate, Vec::new()).is_good());
    }

    #[test]
    fn program_failure_rejects_good_status_lines() {
        let status = format!("{GOOD}[GNUPG:] TRUST_FULLY 0 pgp\n");
        let verification = interpret(status.as_bytes(), false, Trust::Undefined, Vec::new());
        assert_eq!(verification.status, Status::Good);
        assert!(!verification.is_good());
    }

    #[test]
    fn several_signatures_cannot_be_checked() {
        let status =
            format!("{GOOD}[GNUPG:] TRUST_FULLY 0 pgp\n{GOOD}[GNUPG:] TRUST_ULTIMATE 0 pgp\n");
        let verification = interpret(status.as_bytes(), true, Trust::Undefined, Vec::new());
        assert_eq!(verification.status, Status::CannotCheck);
        assert_eq!(verification.signer, None);
        assert_eq!(verification.key, None);
        assert_eq!(verification.trust, Trust::Ultimate);
        assert!(!verification.is_good());
    }

    #[test]
    fn missing_key_cannot_be_checked() {
        let status = b"[GNUPG:] ERRSIG 36F2AA1E2C4A7E1B 22 10 00 1792313552 9 77D6\n\
[GNUPG:] NO_PUBKEY 36F2AA1E2C4A7E1B\n";
        let verification = interpret(status, false, Trust::Undefined, Vec::new());
        assert_eq!(verification.status, Status::CannotCheck);
        assert_eq!(verification.signer, None);
        assert_eq!(verification.key.as_deref(), Some("36F2AA1E2C4A7E1B"));
        assert!(!verification.is_good());
    }
}
