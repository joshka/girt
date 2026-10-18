//! Signed payload splitting compared with what Git hands its signature programs.
//!
//! Every Git call isolates configuration, identity and dates, removes `SSH_AUTH_SOCK`, and points
//! `HOME` and `GNUPGHOME` into the test's temporary directory. Keys are generated per test.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::signing::{
    Programs, SignatureFormat, Signer, SshKey, SshSigner, SshVerifier, Verification,
};
use girt::{Commit, ObjectFormat, Tag};
use rstest::rstest;

fn command(program: &str, directory: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env_remove("SSH_AUTH_SOCK")
        .env("HOME", directory)
        .env("GNUPGHOME", directory.join("gnupg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000");
    command
}

fn run(program: &str, directory: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = command(program, directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("{program} required: {error}"));
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = run("git", directory, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_line(directory: &Path, args: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(directory, args, input))
        .unwrap()
        .trim()
        .to_owned()
}

fn init(root: &Path, format: ObjectFormat) -> std::path::PathBuf {
    let repo = root.join("repo");
    git(
        root,
        &["init", "-q", &format!("--object-format={format}"), "repo"],
        b"",
    );
    repo
}

fn has_program(program: &str, version_arg: &str) -> bool {
    Command::new(program)
        .arg(version_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// What Git passed to its signature program for one verification, if it ran it.
#[cfg(unix)]
struct Capture {
    payload: Vec<u8>,
    signature: Vec<u8>,
}

/// Runs `git verify-<kind> <id>` with every signature program replaced by a recorder.
#[cfg(unix)]
fn capture(root: &Path, repo: &Path, kind: &str, id: &str) -> Option<Capture> {
    use std::os::unix::fs::PermissionsExt;

    let out = root.join("capture");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir(&out).unwrap();
    let script = root.join("record.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             verify=\n\
             for a; do\n\
               case \"$prev\" in -s|--verify) cp \"$a\" '{out}/signature';; esac\n\
               case \"$a\" in --verify|verify|check-novalidate) verify=1;; esac\n\
               prev=$a\n\
             done\n\
             if [ -n \"$verify\" ]; then cat > '{out}/payload'; fi\n\
             exit 1\n",
            out = out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let program = script.to_str().unwrap();
    let allowed = root.join("allowed-signers");
    std::fs::write(&allowed, "").unwrap();
    let output = run(
        "git",
        repo,
        &[
            "-c",
            &format!("gpg.program={program}"),
            "-c",
            &format!("gpg.ssh.program={program}"),
            "-c",
            &format!("gpg.x509.program={program}"),
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            &format!("verify-{kind}"),
            id,
        ],
        b"",
    );
    assert!(!output.status.success());
    let payload = std::fs::read(out.join("payload")).ok()?;
    Some(Capture {
        payload,
        signature: std::fs::read(out.join("signature")).unwrap(),
    })
}

const COMMIT_CASES: [&str; 7] = [
    "gpgsig -----BEGIN PGP SIGNATURE-----\n A\n -----END PGP SIGNATURE-----\n\nmsg\n",
    "gpgsig-sha256 -----BEGIN SSH SIGNATURE-----\n A\n\nmsg\n",
    "gpgsig-sha256 other\ngpgsig -----BEGIN PGP SIGNATURE-----\n A\nx-other y\n\nmsg\n",
    "gpgsig -----BEGIN PGP SIGNATURE-----\n one\ngpgsig -----BEGIN PGP SIGNATURE-----\n two\n\nmsg\n",
    "gpgsig -----BEGIN SIGNED MESSAGE-----\n \n  indented\ngpgsig-sha256 -----BEGIN SIGNED MESSAGE-----\n \n  indented\n\nmsg\n",
    "x-unsigned value\n\nmsg\n",
    "gpgsig -----BEGIN PGP SIGNATURE-----\n one\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n A\ngpgsig -----BEGIN PGP SIGNATURE-----\n A",
];

const TAG_CASES: [&str; 11] = [
    "\nhi\n-----BEGIN SSH SIGNATURE-----\nA\n-----END SSH SIGNATURE-----\n",
    "\nhi\n-----BEGIN PGP SIGNATURE-----\nB\n-----END PGP SIGNATURE-----\nmid\n-----BEGIN SSH SIGNATURE-----\nA\n-----END SSH SIGNATURE-----",
    "\nhi x-----BEGIN SSH SIGNATURE-----\nA\n",
    "\nhi\n-----BEGIN PGP MESSAGE-----\nA\n",
    "\nhi\n-----BEGIN PGP SIGNATURE-----  x\nA\n",
    "\nhi\n-----BEGIN SSH SIGNATUREX-----\nA\n",
    "gpgsig X\n Y\ngpgsig-sha256 Q\n\nmsg\ngpgsig body\n-----BEGIN SIGNED MESSAGE-----\nA\n",
    "-----BEGIN PGP SIGNATURE-----\nA\n",
    "gpgsig A\ngpgsig B\ngpgsig C\n\nmsg\n-----BEGIN PGP SIGNATURE-----\nA\n",
    "gpgsig A\n a\ngpgsig-sha256 B\n b\nx-mid m\ngpgsig-sha256 C\n\nmsg\n-----BEGIN PGP SIGNATURE-----\nA\n",
    "x-pre p\ngpgsig X\n Y\n\nmsg\n-----BEGIN SSH SIGNATURE-----\nA\n",
];

#[cfg(unix)]
#[rstest]
fn commit_payload_matches_git_verify_input(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(0, 1, 2, 3, 4, 5, 6)] case: usize,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), format);
    let tree = git_line(&repo, &["mktree"], b"");
    let bytes = format!(
        "tree {tree}\nauthor A <a@example.com> 1700000000 +0000\ncommitter A <a@example.com> 1700000000 +0000\n{}",
        COMMIT_CASES[case]
    );
    let id = git_line(
        &repo,
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ],
        bytes.as_bytes(),
    );
    let commit = Commit::parse(format, bytes.as_bytes()).unwrap();
    let signed = commit.signed_payload();
    match capture(root.path(), &repo, "commit", &id) {
        Some(captured) => {
            let signed = signed.expect("Git found a signature");
            assert!(signed.format().is_some());
            assert_eq!(signed.payload(), captured.payload);
            assert_eq!(signed.signature(), captured.signature);
        }
        None => assert!(signed.is_none_or(|signed| signed.format().is_none())),
    }
}

#[cfg(unix)]
#[rstest]
fn tag_payload_matches_git_verify_input(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)] case: usize,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), format);
    let tree = git_line(&repo, &["mktree"], b"");
    let bytes = format!(
        "object {tree}\ntype tree\ntag t\ntagger A <a@example.com> 1700000000 +0000\n{}",
        TAG_CASES[case]
    );
    let id = git_line(
        &repo,
        &["hash-object", "-t", "tag", "-w", "--stdin"],
        bytes.as_bytes(),
    );
    let tag = Tag::parse(format, bytes.as_bytes()).unwrap();
    let signed = tag.signed_payload();
    match capture(root.path(), &repo, "tag", &id) {
        Some(captured) => {
            let signed = signed.expect("Git found a signature");
            assert_eq!(signed.payload(), captured.payload);
            assert_eq!(signed.signature(), captured.signature);
        }
        None => assert_eq!(signed, None),
    }
}

/// Signs with a throwaway ssh key through Git, then checks the split with `ssh-keygen -Y verify`.
#[rstest]
fn git_ssh_signed_objects_verify_from_split_payloads(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    if !has_program("ssh-keygen", "-?") {
        eprintln!("skipping: ssh-keygen not installed");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("key");
    let output = run(
        "ssh-keygen",
        root.path(),
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "test",
            "-f",
            key.to_str().unwrap(),
        ],
        b"",
    );
    assert!(output.status.success());
    let public = std::fs::read_to_string(root.path().join("key.pub")).unwrap();
    let allowed = root.path().join("allowed-signers");
    std::fs::write(&allowed, format!("a@example.com {public}")).unwrap();

    let repo = init(root.path(), format);
    let config = [
        ("gpg.format", "ssh".to_owned()),
        ("user.signingKey", key.display().to_string()),
        ("gpg.ssh.allowedSignersFile", allowed.display().to_string()),
    ];
    for (name, value) in &config {
        git(&repo, &["config", name, value], b"");
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "f"], b"");
    git(&repo, &["commit", "-qS", "-m", "base"], b"");
    git(&repo, &["checkout", "-qb", "side"], b"");
    git(
        &repo,
        &["commit", "-qS", "--allow-empty", "-m", "side"],
        b"",
    );
    git(&repo, &["tag", "-s", "-m", "side tag", "side-tag"], b"");
    git(&repo, &["checkout", "-q", "-"], b"");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "main"], b"");
    git(
        &repo,
        &["merge", "-q", "-S", "--no-ff", "-m", "merge", "side-tag"],
        b"",
    );

    let verify = |signed: girt::signing::SignedPayload| {
        assert_eq!(signed.format(), Some(SignatureFormat::Ssh));
        let signature = root.path().join("signature");
        std::fs::write(&signature, signed.signature()).unwrap();
        let output = run(
            "ssh-keygen",
            root.path(),
            &[
                "-Y",
                "verify",
                "-f",
                allowed.to_str().unwrap(),
                "-I",
                "a@example.com",
                "-n",
                "git",
                "-s",
                signature.to_str().unwrap(),
            ],
            signed.payload(),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let merge = Commit::parse(format, &git(&repo, &["cat-file", "commit", "HEAD"], b"")).unwrap();
    assert_eq!(
        merge.id().to_string(),
        git_line(&repo, &["rev-parse", "HEAD"], b"")
    );
    verify(merge.signed_payload().unwrap());

    let tag = Tag::parse(format, &git(&repo, &["cat-file", "tag", "side-tag"], b"")).unwrap();
    verify(tag.signed_payload().unwrap());

    let merged: Vec<Tag> = merge.merge_tags().collect::<Result<_, _>>().unwrap();
    assert_eq!(merged, [tag]);
    verify(merged[0].signed_payload().unwrap());
}

/// A repository with commits and a tag signed through Git by a throwaway ed25519 key.
struct SshRepo {
    root: tempfile::TempDir,
    repo: std::path::PathBuf,
    format: ObjectFormat,
}

impl SshRepo {
    fn new(format: ObjectFormat) -> Option<Self> {
        if !has_program("ssh-keygen", "-?") {
            eprintln!("skipping: ssh-keygen not installed");
            return None;
        }
        let root = tempfile::tempdir().unwrap();
        for (name, comment) in [("key", "test"), ("other", "other")] {
            let path = root.path().join(name);
            let output = run(
                "ssh-keygen",
                root.path(),
                &[
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    comment,
                    "-f",
                    path.to_str().unwrap(),
                ],
                b"",
            );
            assert!(output.status.success());
        }
        let repo = init(root.path(), format);
        let this = Self { root, repo, format };
        this.allow("allowed", "a@example.com", "", "key");
        let key = this.path("key");
        let allowed = this.path("allowed");
        for (name, value) in [
            ("gpg.format", "ssh"),
            ("user.signingKey", key.as_str()),
            ("gpg.ssh.allowedSignersFile", allowed.as_str()),
        ] {
            git(&this.repo, &["config", name, value], b"");
        }
        git(
            &this.repo,
            &["commit", "-qS", "--allow-empty", "-m", "signed"],
            b"",
        );
        git(
            &this.repo,
            &["tag", "-s", "-m", "signed tag", "signed-tag"],
            b"",
        );
        Some(this)
    }

    fn path(&self, name: &str) -> String {
        self.root.path().join(name).display().to_string()
    }

    /// Writes an allowed signers file `name` with one line for `key`'s public key.
    fn allow(&self, name: &str, principals: &str, options: &str, key: &str) {
        let public = std::fs::read_to_string(self.root.path().join(format!("{key}.pub"))).unwrap();
        std::fs::write(
            self.root.path().join(name),
            format!("{principals} {options}{public}"),
        )
        .unwrap();
    }

    fn commit(&self, rev: &str) -> Commit {
        Commit::parse(
            self.format,
            &git(&self.repo, &["cat-file", "commit", rev], b""),
        )
        .unwrap()
    }

    fn tag(&self, rev: &str) -> Tag {
        Tag::parse(
            self.format,
            &git(&self.repo, &["cat-file", "tag", rev], b""),
        )
        .unwrap()
    }

    /// Git's `%G?|%GS|%GF|%GT` for `rev` under extra `-c` settings.
    fn git_verdict(&self, settings: &[String], rev: &str) -> String {
        let mut args: Vec<&str> = settings.iter().flat_map(|s| ["-c", s.as_str()]).collect();
        args.extend(["log", "-1", "--format=%G?|%GS|%GF|%GT", rev]);
        let output = run("git", &self.repo, &args, b"");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn verifier(&self, settings: &[String]) -> Result<SshVerifier, girt::signing::Error> {
        let mut config = format!(
            "[gpg \"ssh\"]\nallowedSignersFile = {}\n",
            self.path("allowed")
        );
        for setting in settings {
            let (name, value) = setting.split_once('=').unwrap();
            let name = name.strip_prefix("gpg.ssh.").unwrap();
            config.push_str(&format!("{name} = {value}\n"));
        }
        let config = girt::Config::parse(config.as_bytes()).unwrap();
        let environment: girt::workflow::Environment =
            [("HOME", self.root.path())].into_iter().collect();
        SshVerifier::from_config(&config, &environment, Programs::Run)
            .map(|verifier| verifier.with_environment(isolated(self.root.path())))
    }
}

/// A program environment with no agent, keyring or real home.
fn isolated(home: &Path) -> girt::workflow::Environment {
    let mut environment: girt::workflow::Environment = [("HOME", home)].into_iter().collect();
    // Windows programs also need SYSTEMROOT to start.
    for name in ["PATH", "SYSTEMROOT"] {
        if let Some(value) = std::env::var_os(name) {
            environment = environment.with(name, value);
        }
    }
    environment.with("GNUPGHOME", home.join("gnupg"))
}

fn verdict(verification: &Verification) -> String {
    format!(
        "{}|{}|{}|{}",
        verification.status.letter(),
        verification.signer.as_deref().unwrap_or(""),
        verification.fingerprint.as_deref().unwrap_or(""),
        verification.trust.as_str()
    )
}

#[rstest]
fn ssh_signatures_match_git_signatures(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let Some(repo) = SshRepo::new(format) else {
        return;
    };
    let signer = SshSigner::new(SshKey::Path(repo.path("key").into()))
        .with_programs(Programs::Run)
        .with_environment(isolated(repo.root.path()));
    assert_eq!(signer.format(), SignatureFormat::Ssh);

    // ed25519 signatures are deterministic, so girt's must equal Git's byte for byte.
    let commit = repo.commit("HEAD");
    let signed = commit.signed_payload().unwrap();
    assert_eq!(signer.sign(signed.payload()).unwrap(), signed.signature());

    let tag = repo.tag("signed-tag");
    let mut fields = tag.to_fields().unwrap();
    let signed = tag.signed_payload().unwrap();
    fields
        .message
        .truncate(fields.message.len() - signed.signature().len());
    let resigned = Tag::new_signed(fields, |payload| signer.sign(payload)).unwrap();
    assert_eq!(resigned, tag);
}

#[test]
fn programs_are_refused_by_default() {
    let signer = SshSigner::new(SshKey::Path("key".into()));
    assert!(matches!(
        signer.sign(b"payload"),
        Err(girt::signing::Error::Refused { .. })
    ));
}

#[rstest]
#[case::good(&[], 'G')]
#[case::unknown_key(&["allowedSignersFile=unknown"], 'U')]
#[case::expired(&["allowedSignersFile=expired"], 'U')]
#[case::not_yet_valid(&["allowedSignersFile=future"], 'U')]
#[case::still_valid(&["allowedSignersFile=window"], 'G')]
#[case::wrong_namespace(&["allowedSignersFile=namespace"], 'B')]
#[case::several_principals(&["allowedSignersFile=several"], 'G')]
#[case::revoked(&["revocationFile=key.pub"], 'B')]
#[case::revoked_unknown(&["allowedSignersFile=unknown", "revocationFile=key.pub"], 'U')]
#[case::missing_file(&["allowedSignersFile=absent"], 'U')]
fn ssh_verification_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] settings: &[&str],
    #[case] letter: char,
    #[values("HEAD", "signed-tag", "tampered")] rev: &str,
) {
    let Some(repo) = SshRepo::new(format) else {
        return;
    };
    repo.allow("unknown", "b@example.com", "", "other");
    repo.allow(
        "expired",
        "a@example.com",
        "valid-before=\"20200101\" ",
        "key",
    );
    repo.allow(
        "future",
        "a@example.com",
        "valid-after=\"20300101\" ",
        "key",
    );
    repo.allow(
        "window",
        "a@example.com",
        "valid-after=\"20231114\",valid-before=\"20231116\" ",
        "key",
    );
    repo.allow("namespace", "a@example.com", "namespaces=\"file\" ", "key");
    repo.allow("several", "a@example.com,c@example.com", "", "key");
    let settings: Vec<String> = settings
        .iter()
        .map(|setting| {
            let (name, file) = setting.split_once('=').unwrap();
            format!("gpg.ssh.{name}={}", repo.path(file))
        })
        .collect();

    let (rev, signed) = match rev {
        "signed-tag" => (rev.to_owned(), repo.tag(rev).signed_payload().unwrap()),
        "tampered" => {
            let bytes = git(&repo.repo, &["cat-file", "commit", "HEAD"], b"");
            let mut tampered = bytes.clone();
            tampered.extend_from_slice(b"tampered\n");
            let id = git_line(
                &repo.repo,
                &["hash-object", "-t", "commit", "-w", "--stdin"],
                &tampered,
            );
            let commit = Commit::parse(format, &tampered).unwrap();
            (id, commit.signed_payload().unwrap())
        }
        _ => (rev.to_owned(), repo.commit(rev).signed_payload().unwrap()),
    };
    // `log --format` shows a tag's target, so compare tags through verify-tag's exit status.
    let verification = repo.verifier(&settings).unwrap().verify(&signed).unwrap();
    if rev == "signed-tag" {
        let mut args: Vec<&str> = settings.iter().flat_map(|s| ["-c", s.as_str()]).collect();
        args.extend(["verify-tag", "signed-tag"]);
        let status = run("git", &repo.repo, &args, b"").status;
        assert_eq!(verification.is_good(), status.success());
    } else {
        assert_eq!(verdict(&verification), repo.git_verdict(&settings, &rev));
    }
    let expected = if rev == "HEAD" || rev == "signed-tag" {
        letter
    } else {
        'B'
    };
    assert_eq!(verification.status.letter(), expected);
}

#[test]
fn ssh_verification_needs_allowed_signers() {
    let config = girt::Config::parse(b"").unwrap();
    let verifier = SshVerifier::from_config(&config, &Default::default(), Programs::Run).unwrap();
    let tag = Tag::parse(
        ObjectFormat::Sha1,
        format!(
            "object {}\ntype commit\ntag t\n\n-----BEGIN SSH SIGNATURE-----\n",
            "1".repeat(40)
        )
        .as_bytes(),
    )
    .unwrap();
    assert!(matches!(
        verifier.verify(&tag.signed_payload().unwrap()),
        Err(girt::signing::Error::NoAllowedSigners)
    ));
}
