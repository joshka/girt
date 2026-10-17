use super::{CommitError, Signature};
use crate::Config;
use crate::workflow::Environment;

impl Signature {
    /// Creates a sanitized identity at the current time in the system's local time zone.
    ///
    /// Wraps explicit [`Signature`] construction: removes angle brackets, CR, LF and NUL, trims
    /// ASCII whitespace and boundary quotes, commas, colons, semicolons and backslashes, and clamps
    /// the offset to ±1439 minutes. Bytes need not be UTF-8. If the system time zone cannot be
    /// discovered, Jiff uses UTC.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] when either sanitized component is empty.
    ///
    /// ```
    /// let signature = girt::Signature::now(b" A. Writer ", b"writer@example.com")?;
    /// assert_eq!(signature.name, b"A. Writer");
    /// # Ok::<(), girt::CommitError>(())
    /// ```
    pub fn now(name: impl AsRef<[u8]>, email: impl AsRef<[u8]>) -> Result<Self, CommitError> {
        let now = jiff::Zoned::now();
        let signature = Self {
            name: sanitize(name.as_ref()),
            email: sanitize(email.as_ref()),
            seconds: now.timestamp().as_second(),
            offset_minutes: (now.offset().seconds() / 60).clamp(-1439, 1439) as i16,
        };
        signature.validate()?;
        Ok(signature)
    }

    /// Builds an author from an explicit environment snapshot and effective configuration.
    ///
    /// `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL` override `user.name` and `user.email`
    /// independently, including empty values, then role-specific `author.*` config precedes
    /// `user.*`. The `EMAIL` environment variable is the final email fallback. Load Git's usual
    /// configuration sources with [`crate::config::ConfigInputs::git_default`]. No OS username or
    /// hostname is guessed. This wraps [`Self::now`] and explicit date assignment.
    /// `GIT_AUTHOR_DATE`, when present, must be raw `@SECONDS ±HHMM` or RFC 3339; other date
    /// formats are refused. Offsets are clamped to ±1439 minutes, even though Git preserves
    /// larger raw offsets. Empty email is refused by construction validation even though Git
    /// accepts it. An absent date uses the current system time and local zone; the snapshot
    /// does not control system time-zone discovery.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] for missing or empty sanitized identity fields,
    /// or [`CommitError::InvalidDate`] for an unsupported or malformed date.
    ///
    /// ```
    /// use girt::workflow::Environment;
    /// use girt::{Config, Signature};
    /// let config = Config::parse(b"[user]\nname = Writer\nemail = writer@example.com\n")?;
    /// let environment = Environment::default().with("GIT_AUTHOR_DATE", "@1700000000 +0130");
    /// let author = Signature::author(&config, &environment)?;
    /// assert_eq!((author.seconds, author.offset_minutes), (1700000000, 90));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn author(config: &Config, environment: &Environment) -> Result<Self, CommitError> {
        configured(
            config,
            environment,
            "author",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
        )
    }

    /// Builds a committer using `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL` and
    /// `GIT_COMMITTER_DATE`, then `committer.*`, `user.*` configuration and the `EMAIL` fallback.
    ///
    /// Wraps [`Self::now`] and explicit date assignment with the same sanitization, date format,
    /// offset clamping and system-zone policy as [`Self::author`].
    ///
    /// # Errors
    ///
    /// Returns the identity and date errors documented on [`Self::author`].
    pub fn committer(config: &Config, environment: &Environment) -> Result<Self, CommitError> {
        configured(
            config,
            environment,
            "committer",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
        )
    }
}

fn sanitize(bytes: &[u8]) -> Vec<u8> {
    let bytes: Vec<_> = bytes
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, 0 | b'\r' | b'\n' | b'<' | b'>'))
        .collect();
    let boundary = |byte: &u8| {
        byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b',' | b':' | b';' | b'\\')
    };
    let start = bytes
        .iter()
        .position(|byte| !boundary(byte))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !boundary(byte))
        .map_or(start, |index| index + 1);
    bytes.get(start..end).unwrap_or_default().to_vec()
}

fn configured(
    config: &Config,
    environment: &Environment,
    role: &str,
    name: &str,
    email: &str,
    date: &str,
) -> Result<Signature, CommitError> {
    let lookup = |variable, key| {
        environment
            .get(variable)
            .map(|value| value.as_encoded_bytes())
            .or_else(|| config.string(role, None, key))
            .or_else(|| config.string("user", None, key))
            .or_else(|| {
                (key == "email")
                    .then(|| environment.get("EMAIL"))
                    .flatten()
                    .map(|value| value.as_encoded_bytes())
            })
            .ok_or(CommitError::InvalidSignature)
    };
    let mut signature = Signature::now(lookup(name, "name")?, lookup(email, "email")?)?;
    if let Some(value) = environment.get(date) {
        let text = value.to_str().ok_or(CommitError::InvalidDate)?;
        if !text.starts_with('@') {
            let timestamp: jiff::Timestamp = text.parse().map_err(|_| CommitError::InvalidDate)?;
            let offset = if text.ends_with('Z') || text.ends_with('z') {
                0
            } else {
                let suffix = text
                    .get(text.len().saturating_sub(6)..)
                    .ok_or(CommitError::InvalidDate)?;
                let hours: i16 = suffix
                    .get(..3)
                    .ok_or(CommitError::InvalidDate)?
                    .parse()
                    .map_err(|_| CommitError::InvalidDate)?;
                let minutes: i16 = suffix
                    .get(4..)
                    .ok_or(CommitError::InvalidDate)?
                    .parse()
                    .map_err(|_| CommitError::InvalidDate)?;
                hours * 60
                    + if suffix.starts_with('-') {
                        -minutes
                    } else {
                        minutes
                    }
            };
            signature.seconds = timestamp.as_second();
            signature.offset_minutes = offset.clamp(-1439, 1439);
            return Ok(signature);
        }
        let raw = text.strip_prefix('@').ok_or(CommitError::InvalidDate)?;
        let mut parts = raw.split(' ');
        let seconds = parts.next().ok_or(CommitError::InvalidDate)?;
        let offset = parts.next().ok_or(CommitError::InvalidDate)?;
        if parts.next().is_some()
            || seconds.is_empty()
            || !seconds.bytes().all(|b| b.is_ascii_digit())
            || offset.len() != 5
            || !matches!(offset.as_bytes().first(), Some(b'+' | b'-'))
            || !offset
                .as_bytes()
                .get(1..)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
        {
            return Err(CommitError::InvalidDate);
        }
        signature.seconds = seconds.parse().map_err(|_| CommitError::InvalidDate)?;
        let hhmm: i32 = offset.parse().map_err(|_| CommitError::InvalidDate)?;
        signature.offset_minutes = (hhmm / 100 * 60 + hhmm % 100).clamp(-1439, 1439) as i16;
    }
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    // Oracle: installed Git executable; no upstream implementation or fixtures are used.
    #[test]
    fn configured_identities_match_git() {
        let directory = tempfile::tempdir().unwrap();
        let config =
            Config::parse(b"[user]\nname = Config Writer\nemail = config@example.com\n").unwrap();
        for role in ["AUTHOR", "COMMITTER"] {
            for (name, email) in [
                (" Override <Writer>\n ", " <writer@example.com>\r "),
                ("Byte Writer", "writer@example.com"),
                ("\"Q\";", "writer@example.com"),
            ] {
                let environment = Environment::default()
                    .with(format!("GIT_{role}_NAME"), name)
                    .with(format!("GIT_{role}_EMAIL"), email)
                    .with(format!("GIT_{role}_DATE"), "@1700000000 -0730");
                let actual = if role == "AUTHOR" {
                    Signature::author(&config, &environment)
                } else {
                    Signature::committer(&config, &environment)
                }
                .unwrap();
                let output = Command::new("git")
                    .current_dir(directory.path())
                    .env_clear()
                    .env("PATH", std::env::var_os("PATH").unwrap())
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", directory.path().join("absent"))
                    .env(format!("GIT_{role}_NAME"), name)
                    .env(format!("GIT_{role}_EMAIL"), email)
                    .env(format!("GIT_{role}_DATE"), "@1700000000 -0730")
                    .args(["var", &format!("GIT_{role}_IDENT")])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{:?}", output);
                assert_eq!(
                    actual,
                    Signature::parse(output.stdout.trim_ascii_end()).unwrap()
                );
            }
        }
    }

    #[test]
    fn role_config_and_iso_dates_match_git() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = b"[user]\nname = User\nemail = user@example.com\n[author]\nname = Author\nemail = author@example.com\n[committer]\nname = Committer\nemail = committer@example.com\n";
        let path = directory.path().join("config");
        std::fs::write(&path, bytes).unwrap();
        let config = Config::parse(bytes).unwrap();
        for role in ["AUTHOR", "COMMITTER"] {
            for date in [
                "2026-01-02T03:04:05+01:00",
                "2026-01-02T03:04:05-00:30",
                "2026-01-02T03:04:05Z",
            ] {
                let environment = Environment::default().with(format!("GIT_{role}_DATE"), date);
                let actual = if role == "AUTHOR" {
                    Signature::author(&config, &environment)
                } else {
                    Signature::committer(&config, &environment)
                }
                .unwrap();
                let output = Command::new("git")
                    .current_dir(directory.path())
                    .env_clear()
                    .env("PATH", std::env::var_os("PATH").unwrap())
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", &path)
                    .env(format!("GIT_{role}_DATE"), date)
                    .args(["var", &format!("GIT_{role}_IDENT")])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{:?}", output);
                assert_eq!(
                    actual,
                    Signature::parse(output.stdout.trim_ascii_end()).unwrap()
                );
            }
        }
        let config = Config::parse(b"[user]\nname = A\n").unwrap();
        assert_eq!(
            Signature::author(
                &config,
                &Environment::default().with("EMAIL", "fallback@example.com")
            )
            .unwrap()
            .email,
            b"fallback@example.com"
        );
    }

    #[test]
    fn fallback_is_per_field_and_empty_overrides_fail() {
        let config =
            Config::parse(b"[user]\nname = Config Writer\nemail = config@example.com\n").unwrap();
        let environment = Environment::default().with("GIT_AUTHOR_EMAIL", "author@example.com");
        let author = Signature::author(&config, &environment).unwrap();
        assert_eq!(author.name, b"Config Writer");
        assert_eq!(author.email, b"author@example.com");
        assert!(Signature::author(&config, &environment.with("GIT_AUTHOR_NAME", "<>\n")).is_err());
        assert!(
            Signature::committer(&Config::parse(b" ").unwrap(), &Environment::default()).is_err()
        );
        assert_eq!(sanitize(b" \0 A<>\r\n \0 "), b"A");
    }

    #[test]
    fn dates_are_explicit_and_offsets_are_clamped() {
        // Clamping is girt construction policy: Git preserves raw +2500 and -2500.
        let config = Config::parse(b"[user]\nname = A\nemail = a@b\n").unwrap();
        for (date, expected) in [
            ("@1700000000 +2500", 1439),
            ("@1700000000 -2500", -1439),
            ("@1700000000 +0130", 90),
        ] {
            let signature = Signature::committer(
                &config,
                &Environment::default().with("GIT_COMMITTER_DATE", date),
            )
            .unwrap();
            assert_eq!(signature.offset_minutes, expected);
        }
        for date in ["", "yesterday", "@1 +0000 extra", "@x +0000", "@1 +00xx"] {
            assert_eq!(
                Signature::author(
                    &config,
                    &Environment::default().with("GIT_AUTHOR_DATE", date)
                ),
                Err(CommitError::InvalidDate)
            );
        }
        let before = jiff::Timestamp::now().as_second();
        let signature = Signature::now(b"A", b"a@b").unwrap();
        assert!((before..=jiff::Timestamp::now().as_second()).contains(&signature.seconds));
        signature.validate().unwrap();
    }
}
