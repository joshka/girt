//! OpenPGP and X.509 signing and verification compared with Git.
//!
//! Every program runs with an explicit environment: configuration, identity and dates are fixed,
//! `SSH_AUTH_SOCK` is absent, and `HOME` and `GNUPGHOME` point into the test's temporary
//! directory. Keys are generated per test, and the agents started for those keyrings are stopped
//! when the test ends. Tests skip when `gpg` or `gpgsm` is not installed.

// Unix only: Git for Windows' MSYS gpg reads a native `GNUPGHOME` as a relative POSIX path and
// cannot generate keys. Running these on Windows needs MSYS path translation:
// https://github.com/joshka/girt/issues/690.
#![cfg(unix)]

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use girt::signing::{
    GpgSigner, GpgVerifier, Programs, SignatureFormat, Signer, Status, Trust, Verification,
    Verifier,
};
use girt::workflow::Environment;
use girt::{Commit, ObjectFormat, Tag};
use rstest::rstest;

/// A temporary directory holding a repository and one or more keyrings.
struct Fixture {
    root: tempfile::TempDir,
    homes: std::cell::RefCell<Vec<PathBuf>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for home in self.homes.borrow().iter() {
            let _ = Command::new("gpgconf")
                .args(["--kill", "all"])
                .env("GNUPGHOME", home)
                .output();
        }
    }
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            homes: Default::default(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// A keyring directory `name`, created on first use.
    fn home(&self, name: &str) -> PathBuf {
        let home = self.path(name);
        if !home.exists() {
            std::fs::create_dir(&home).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            self.homes.borrow_mut().push(home.clone());
        }
        home
    }

    /// The environment every program runs with, using keyring `home`.
    fn environment(&self, home: &str) -> Environment {
        let mut environment: Environment = [
            ("HOME", self.root.path().as_os_str().to_owned()),
            ("GNUPGHOME", self.home(home).into_os_string()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            (
                "GIT_CONFIG_GLOBAL",
                self.path("absent-config").into_os_string(),
            ),
            ("GIT_AUTHOR_NAME", "A".into()),
            ("GIT_AUTHOR_EMAIL", "a@example.com".into()),
            ("GIT_AUTHOR_DATE", "@1700000000 +0000".into()),
            ("GIT_COMMITTER_NAME", "A".into()),
            ("GIT_COMMITTER_EMAIL", "a@example.com".into()),
            ("GIT_COMMITTER_DATE", "@1700000000 +0000".into()),
        ]
        .into_iter()
        .map(|(name, value): (&str, OsString)| (name, value))
        .collect();
        for name in ["PATH", "SYSTEMROOT", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                environment = environment.with(name, value);
            }
        }
        environment
    }

    fn run(&self, home: &str, program: &str, args: &[&str], input: &[u8]) -> Output {
        let mut command = Command::new(program);
        command.env_clear();
        for (name, value) in [
            "HOME",
            "GNUPGHOME",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
            "PATH",
            "SYSTEMROOT",
            "TMPDIR",
        ]
        .into_iter()
        .filter_map(|name| Some((name, self.environment(home).get(name)?.to_owned())))
        {
            command.env(name, value);
        }
        let mut child = command
            .current_dir(self.path("repo"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("{program}: {error}"));
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, home: &str, program: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
        let output = self.run(home, program, args, input);
        assert!(
            output.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git_line(&self, args: &[&str], input: &[u8]) -> String {
        String::from_utf8(self.ok("signer", "git", args, input))
            .unwrap()
            .trim()
            .to_owned()
    }

    /// Git's `%G?|%GS|%GK|%GF|%GP|%GT` for `rev` and whether `verify-commit` accepts it.
    fn git_verdict(&self, home: &str, settings: &[&str], rev: &str) -> (String, bool) {
        let mut args: Vec<&str> = settings.iter().flat_map(|s| ["-c", *s]).collect();
        let mut log = args.clone();
        log.extend(["log", "-1", "--format=%G?|%GS|%GK|%GF|%GP|%GT", rev]);
        let verdict = String::from_utf8(self.run(home, "git", &log, b"").stdout).unwrap();
        args.extend(["verify-commit", rev]);
        let accepted = self.run(home, "git", &args, b"").status.success();
        (verdict.trim().to_owned(), accepted)
    }

    fn commit(&self, format: ObjectFormat, rev: &str) -> Commit {
        Commit::parse(
            format,
            &self.ok("signer", "git", &["cat-file", "commit", rev], b""),
        )
        .unwrap()
    }
}

/// The fingerprint in a `--with-colons` `fpr` record, which is its tenth field.
fn fingerprint_field(line: &str) -> Option<String> {
    let mut fields = line.split(':');
    (fields.next() == Some("fpr"))
        .then(|| fields.nth(8))?
        .map(str::to_owned)
}

fn has_program(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn verdict(verification: &Verification) -> String {
    let field = |value: &Option<String>| value.clone().unwrap_or_default();
    format!(
        "{}|{}|{}|{}|{}|{}",
        verification.status.letter(),
        field(&verification.signer),
        field(&verification.key),
        field(&verification.fingerprint),
        field(&verification.primary_fingerprint),
        verification.trust.as_str()
    )
}

/// A repository whose `signer` keyring holds an ultimately trusted OpenPGP key for
/// `a@example.com`, and a commit Git signed with it.
fn openpgp(format: ObjectFormat) -> Option<(Fixture, String)> {
    if !has_program("gpg") {
        eprintln!("skipping: gpg not installed");
        return None;
    }
    let fixture = Fixture::new();
    fixture.home("signer");
    std::fs::create_dir(fixture.path("repo")).unwrap();
    fixture.ok(
        "signer",
        "gpg",
        &[
            "--batch",
            "--passphrase",
            "",
            "--quick-gen-key",
            "A <a@example.com>",
            "ed25519",
            "sign",
            "never",
        ],
        b"",
    );
    let fingerprint = String::from_utf8(fixture.ok(
        "signer",
        "gpg",
        &["--with-colons", "--list-keys", "a@example.com"],
        b"",
    ))
    .unwrap()
    .lines()
    .find_map(fingerprint_field)
    .unwrap();
    fixture.ok(
        "signer",
        "git",
        &["init", "-q", &format!("--object-format={format}"), "."],
        b"",
    );
    for (name, value) in [
        ("user.signingKey", "a@example.com"),
        ("gpg.format", "openpgp"),
    ] {
        fixture.ok("signer", "git", &["config", name, value], b"");
    }
    fixture.ok(
        "signer",
        "git",
        &["commit", "-qS", "--allow-empty", "-m", "signed"],
        b"",
    );
    Some((fixture, fingerprint))
}

/// Exports the signer's public key into keyring `home`, with `trust` as direct ownertrust.
fn import(fixture: &Fixture, home: &str, fingerprint: &str, trust: Option<u8>) {
    let public = fixture.ok("signer", "gpg", &["--export", "a@example.com"], b"");
    fixture.home(home);
    fixture.ok(home, "gpg", &["--batch", "--import"], &public);
    if let Some(trust) = trust {
        std::fs::write(fixture.home(home).join("gpg.conf"), "trust-model direct\n").unwrap();
        let line = format!("{fingerprint}:{trust}:\n");
        fixture.ok(
            home,
            "gpg",
            &["--batch", "--import-ownertrust"],
            line.as_bytes(),
        );
    }
}

#[rstest]
fn girt_and_git_openpgp_signatures_verify_each_other(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let Some((fixture, _)) = openpgp(format) else {
        return;
    };
    let environment = fixture.environment("signer");
    let config = girt::Config::parse(b"[user]\nsigningKey = a@example.com\n").unwrap();
    let signer = GpgSigner::from_config(
        &config,
        &environment,
        SignatureFormat::OpenPgp,
        Programs::Run,
    )
    .unwrap()
    .with_environment(environment.clone());

    // girt signs what Git would sign, and Git accepts it.
    let git_signed = fixture.commit(format, "HEAD");
    let signed = git_signed.signed_payload().unwrap();
    let signature = signer.sign(signed.payload()).unwrap();
    assert_eq!(
        SignatureFormat::detect(&signature),
        Some(SignatureFormat::OpenPgp)
    );
    let fields = Commit::parse(format, signed.payload())
        .unwrap()
        .to_fields()
        .unwrap();
    let ours =
        girt::commit::CommitBuilder::new(fields.tree, fields.author.clone(), &fields.message)
            .committer(fields.committer.clone())
            .build_signed(|payload| signer.sign(payload))
            .unwrap();
    let id = fixture.git_line(
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        ours.as_bytes(),
    );
    assert_eq!(id, ours.id().to_string());
    let (git, accepted) = fixture.git_verdict("signer", &[], &id);
    assert!(accepted, "{git}");

    // girt reads Git's signature as Git does.
    let verifier = Verifier::from_config(&config, &environment, Programs::Run)
        .unwrap()
        .with_environment(environment);
    for id in [id.as_str(), "HEAD"] {
        let verification = verifier
            .verify(&fixture.commit(format, id).signed_payload().unwrap())
            .unwrap();
        let (git, accepted) = fixture.git_verdict("signer", &[], id);
        assert_eq!(verdict(&verification), git);
        assert_eq!(verification.is_good(), accepted);
        assert_eq!(verification.status, Status::Good);
    }
}

#[rstest]
#[case::unknown_validity(None, None)]
#[case::never(Some(3), None)]
#[case::marginal(Some(4), None)]
#[case::full(Some(5), None)]
#[case::marginal_below_minimum(Some(4), Some("fully"))]
#[case::full_at_minimum(Some(5), Some("FULLY"))]
#[case::full_below_minimum(Some(5), Some("ultimate"))]
#[case::unknown_below_minimum(None, Some("marginal"))]
fn openpgp_trust_matches_git(#[case] ownertrust: Option<u8>, #[case] minimum: Option<&str>) {
    let Some((fixture, fingerprint)) = openpgp(ObjectFormat::Sha1) else {
        return;
    };
    import(&fixture, "verifier", &fingerprint, ownertrust);
    let setting = minimum.map(|level| format!("gpg.minTrustLevel={level}"));
    let settings: Vec<&str> = setting.iter().map(String::as_str).collect();
    let config = minimum
        .map(|level| format!("[gpg]\nminTrustLevel = {level}\n"))
        .unwrap_or_default();
    let config = girt::Config::parse(config.as_bytes()).unwrap();
    let verifier = GpgVerifier::from_config(&config, Programs::Run)
        .unwrap()
        .with_environment(fixture.environment("verifier"));
    let verification = verifier
        .verify(
            &fixture
                .commit(ObjectFormat::Sha1, "HEAD")
                .signed_payload()
                .unwrap(),
        )
        .unwrap();
    let (git, accepted) = fixture.git_verdict("verifier", &settings, "HEAD");
    assert_eq!(verdict(&verification), git);
    assert_eq!(verification.is_good(), accepted);
}

#[test]
fn missing_key_and_tampering_match_git() {
    let Some((fixture, _)) = openpgp(ObjectFormat::Sha1) else {
        return;
    };
    let mut tampered = fixture.ok("signer", "git", &["cat-file", "commit", "HEAD"], b"");
    tampered.extend_from_slice(b"tampered\n");
    let id = fixture.git_line(&["hash-object", "-t", "commit", "-w", "--stdin"], &tampered);
    fixture.home("empty");
    for (home, rev, status) in [
        ("empty", "HEAD", Status::CannotCheck),
        ("signer", id.as_str(), Status::Bad),
    ] {
        let verification = GpgVerifier::new()
            .with_programs(Programs::Run)
            .with_environment(fixture.environment(home))
            .verify(
                &fixture
                    .commit(ObjectFormat::Sha1, rev)
                    .signed_payload()
                    .unwrap(),
            )
            .unwrap();
        let (git, accepted) = fixture.git_verdict(home, &[], rev);
        assert_eq!(verification.status, status);
        assert_eq!(verdict(&verification), git);
        assert_eq!(verification.is_good(), accepted);
    }
}

/// Signs `HEAD` with gpg pretending an earlier date, for expired keys and signatures.
#[cfg(unix)]
fn sign_in_past(fixture: &Fixture, key: &str, created: &str, signed: &str, extra: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    fixture.ok(
        "signer",
        "gpg",
        &[
            "--batch",
            "--passphrase",
            "",
            "--faked-system-time",
            created,
            "--quick-gen-key",
            &format!("{key} <{key}@example.com>"),
            "ed25519",
            "sign",
            if key == "expiring" { "1d" } else { "never" },
        ],
        b"",
    );
    let script = fixture.path(&format!("{key}.sh"));
    std::fs::write(
        &script,
        format!("#!/bin/sh\nexec gpg --faked-system-time {signed} {extra} \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture.ok(
        "signer",
        "git",
        &[
            "-c",
            &format!("gpg.program={}", script.display()),
            "-c",
            &format!("user.signingKey={key}@example.com"),
            "commit",
            "-qS",
            "--allow-empty",
            "-m",
            key,
        ],
        b"",
    );
    fixture.git_line(&["rev-parse", "HEAD"], b"")
}

#[cfg(unix)]
#[test]
fn expired_and_revoked_keys_match_git() {
    let Some((fixture, _)) = openpgp(ObjectFormat::Sha1) else {
        return;
    };
    let expired_key = sign_in_past(
        &fixture,
        "expiring",
        "20200101T000000",
        "20200101T010000",
        "",
    );
    let expired_signature = sign_in_past(
        &fixture,
        "dated",
        "20190101T000000",
        "20200101T000000",
        "--default-sig-expire 1d",
    );
    fixture.ok(
        "signer",
        "git",
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "revoked",
            "-S",
            "--gpg-sign=a@example.com",
        ],
        b"",
    );
    let revoked = fixture.git_line(&["rev-parse", "HEAD"], b"");
    let fingerprint = String::from_utf8(fixture.ok(
        "signer",
        "gpg",
        &["--with-colons", "--list-keys", "a@example.com"],
        b"",
    ))
    .unwrap()
    .lines()
    .find_map(fingerprint_field)
    .unwrap();
    let certificate = std::fs::read_to_string(
        fixture
            .home("signer")
            .join("openpgp-revocs.d")
            .join(format!("{fingerprint}.rev")),
    )
    .unwrap()
    .replace(":-----BEGIN", "-----BEGIN");
    fixture.ok(
        "signer",
        "gpg",
        &["--batch", "--import"],
        certificate.as_bytes(),
    );

    let verifier = GpgVerifier::new()
        .with_programs(Programs::Run)
        .with_environment(fixture.environment("signer"));
    for (rev, status) in [
        (expired_key.as_str(), Status::ExpiredKey),
        (expired_signature.as_str(), Status::ExpiredSignature),
        (revoked.as_str(), Status::RevokedKey),
    ] {
        let verification = verifier
            .verify(
                &fixture
                    .commit(ObjectFormat::Sha1, rev)
                    .signed_payload()
                    .unwrap(),
            )
            .unwrap();
        let (git, accepted) = fixture.git_verdict("signer", &[], rev);
        assert_eq!(verification.status, status, "{git}");
        assert_eq!(verdict(&verification), git);
        assert_eq!(verification.is_good(), accepted);
    }
}

#[test]
fn openpgp_signed_tags_verify_like_git() {
    let Some((fixture, _)) = openpgp(ObjectFormat::Sha1) else {
        return;
    };
    fixture.ok(
        "signer",
        "git",
        &["tag", "-s", "-m", "signed tag", "v1"],
        b"",
    );
    let tag = Tag::parse(
        ObjectFormat::Sha1,
        &fixture.ok("signer", "git", &["cat-file", "tag", "v1"], b""),
    )
    .unwrap();
    let verification = GpgVerifier::new()
        .with_programs(Programs::Run)
        .with_environment(fixture.environment("signer"))
        .verify(&tag.signed_payload().unwrap())
        .unwrap();
    assert_eq!(verification.status, Status::Good);
    assert_eq!(verification.trust, Trust::Ultimate);
    let accepted = fixture
        .run("signer", "git", &["verify-tag", "v1"], b"")
        .status
        .success();
    assert_eq!(verification.is_good(), accepted);
}

#[test]
fn x509_signatures_match_git() {
    if !has_program("gpgsm") {
        eprintln!("skipping: gpgsm not installed");
        return;
    }
    let fixture = Fixture::new();
    fixture.home("signer");
    std::fs::create_dir(fixture.path("repo")).unwrap();
    // gpgsm 2.4 ignores %no-protection. Supply an empty passphrase through loopback
    // instead, keeping the key parameters in a separate file so stdin carries only it.
    let parameters = fixture.path("x509-parameters");
    std::fs::write(
        &parameters,
        b"Key-Type: RSA\nKey-Length: 2048\nKey-Usage: sign, cert\nSerial: random\n\
Name-DN: CN=Test,O=girt\nName-Email: a@example.com\n",
    )
    .unwrap();
    let certificate = fixture.ok(
        "signer",
        "gpgsm",
        &[
            "--batch",
            "--armor",
            "--pinentry-mode",
            "loopback",
            "--passphrase-fd",
            "0",
            "--gen-key",
            parameters.to_str().unwrap(),
        ],
        b"\n",
    );
    fixture.ok("signer", "gpgsm", &["--batch", "--import"], &certificate);
    let fingerprint =
        String::from_utf8(fixture.ok("signer", "gpgsm", &["--with-colons", "--list-keys"], b""))
            .unwrap()
            .lines()
            .find_map(fingerprint_field)
            .unwrap();
    let home = fixture.home("signer");
    std::fs::write(
        home.join("trustlist.txt"),
        format!("{fingerprint} S relax\n"),
    )
    .unwrap();
    std::fs::write(home.join("gpgsm.conf"), "disable-crl-checks\n").unwrap();
    fixture.ok("signer", "git", &["init", "-q", "."], b"");
    for (name, value) in [
        ("user.signingKey", fingerprint.as_str()),
        ("gpg.format", "x509"),
    ] {
        fixture.ok("signer", "git", &["config", name, value], b"");
    }
    fixture.ok(
        "signer",
        "git",
        &["commit", "-qS", "--allow-empty", "-m", "x509"],
        b"",
    );

    let environment = fixture.environment("signer");
    let config = girt::Config::parse(
        format!("[user]\nsigningKey = {fingerprint}\n[gpg]\nformat = x509\n").as_bytes(),
    )
    .unwrap();
    let settings = girt::signing::Settings::from_config(&config).unwrap();
    assert_eq!(settings.format, SignatureFormat::X509);
    let signer = settings
        .signer(&config, &environment, Programs::Run)
        .unwrap();
    assert_eq!(signer.format(), SignatureFormat::X509);
    let signed = fixture
        .commit(ObjectFormat::Sha1, "HEAD")
        .signed_payload()
        .unwrap();
    assert_eq!(signed.format(), Some(SignatureFormat::X509));

    let verifier = Verifier::from_config(&config, &environment, Programs::Run)
        .unwrap()
        .with_environment(environment);
    let verification = verifier.verify(&signed).unwrap();
    let (git, accepted) = fixture.git_verdict("signer", &[], "HEAD");
    assert_eq!(verdict(&verification), git);
    assert_eq!(verification.is_good(), accepted);
    assert_eq!(verification.status, Status::Good);
}

#[cfg(unix)]
#[test]
fn several_signatures_match_git() {
    use std::os::unix::fs::PermissionsExt;

    let Some((fixture, _)) = openpgp(ObjectFormat::Sha1) else {
        return;
    };
    fixture.ok(
        "signer",
        "gpg",
        &[
            "--batch",
            "--passphrase",
            "",
            "--quick-gen-key",
            "B <b@example.com>",
            "ed25519",
            "sign",
            "never",
        ],
        b"",
    );
    // A second `-u` makes gpg put two signature packets in one armored signature.
    let script = fixture.path("two.sh");
    std::fs::write(&script, "#!/bin/sh\nexec gpg -u b@example.com \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let program = format!("gpg.program={}", script.display());
    fixture.ok(
        "signer",
        "git",
        &[
            "-c",
            &program,
            "commit",
            "-qS",
            "--allow-empty",
            "-m",
            "two",
        ],
        b"",
    );
    let verification = GpgVerifier::new()
        .with_programs(Programs::Run)
        .with_environment(fixture.environment("signer"))
        .verify(
            &fixture
                .commit(ObjectFormat::Sha1, "HEAD")
                .signed_payload()
                .unwrap(),
        )
        .unwrap();
    let (git, accepted) = fixture.git_verdict("signer", &[], "HEAD");
    assert_eq!(verification.status, Status::CannotCheck);
    assert_eq!(verdict(&verification), git);
    assert_eq!(verification.is_good(), accepted);
}
