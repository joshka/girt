//! Application transports: a `mem::<path>` scheme served by `git upload-pack` through
//! `ChildConnection`, compared with `git ls-remote` on the same repository.
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::process::{Command, Stdio};
use std::sync::Arc;

use girt::Repository;
use girt::transport::{
    ChildConnection, ConnectError, ConnectRequest, Connection, Service, Transport,
};
use girt::workflow::{Environment, TransferCallbacks, TransferError};
use pack_git::{Fixture, git};
use rstest::rstest;

// Runs the requested Git service on the path after `mem::`, with isolated configuration.
struct GitService;

impl Transport for GitService {
    fn connect(&self, request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError> {
        assert_eq!(request.scheme(), b"mem");
        assert_eq!(request.service(), Service::UploadPack);
        let path = String::from_utf8(request.address().to_vec()).unwrap();
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        if let Some(protocol) = request.protocol().git_protocol() {
            command.env("GIT_PROTOCOL", protocol);
        }
        let child = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/nonexistent/girt-test-config")
            .args(["upload-pack", "--strict", &path])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A v2 service reports the session girt closes after listing as a hang-up.
            .stderr(Stdio::null())
            .spawn()
            .map_err(ConnectError::new)?;
        Ok(Box::new(ChildConnection::new(child)))
    }
}

struct Refusing;

impl Transport for Refusing {
    fn connect(&self, _request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError> {
        Err(ConnectError::new("vault is sealed"))
    }
}

struct Registered(Option<Arc<dyn Transport>>);

impl TransferCallbacks for Registered {
    fn transport(&self, scheme: &[u8]) -> Option<Arc<dyn Transport>> {
        (scheme == b"mem").then(|| self.0.clone()).flatten()
    }
}

// A repository whose `origin` is `mem::<fixture>`, with `extra` configuration.
fn local(fixture: &Fixture, extra: &[(&str, &str)]) -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "--quiet", "."], b"");
    let url = format!("mem::{}", fixture.root.path().display());
    git(directory.path(), &["remote", "add", "origin", &url], b"");
    for (name, value) in extra {
        git(directory.path(), &["config", name, value], b"");
    }
    let repository = Repository::open(directory.path()).unwrap();
    (directory, repository)
}

#[rstest]
#[case::v0("0")]
#[case::v2("2")]
fn remote_refs_match_git_ls_remote(#[case] version: &str) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(
        &fixture,
        &[
            ("protocol.mem.allow", "always"),
            ("protocol.version", version),
        ],
    );
    let mut callbacks = Registered(Some(Arc::new(GitService)));
    let listing = girt::workflow::remote_refs(
        &repository,
        "origin",
        &Environment::default(),
        &mut callbacks,
    )
    .unwrap();
    let mut girt_lines: Vec<String> = listing
        .advertisement
        .refs
        .iter()
        .map(|reference| {
            let suffix = match reference.kind {
                girt::fetch::AdvertisementKind::PeeledHint => "^{}",
                _ => "",
            };
            format!(
                "{}\t{}{suffix}",
                reference.id,
                String::from_utf8_lossy(reference.name.as_bytes())
            )
        })
        .collect();
    let git_output = git(fixture.root.path(), &["ls-remote", "."], b"");
    let mut git_lines: Vec<String> = String::from_utf8(git_output)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    girt_lines.sort();
    git_lines.sort();
    assert_eq!(girt_lines, git_lines);
    let expected = if version == "2" {
        girt::fetch::ProtocolVersion::V2
    } else {
        girt::fetch::ProtocolVersion::V0
    };
    assert_eq!(listing.version, expected);
}

#[test]
fn unregistered_scheme_is_unsupported() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &Environment::default(),
        &mut Registered(None),
    );
    assert!(matches!(
        result,
        Err(TransferError::UnsupportedTransport(_))
    ));
}

#[test]
fn protocol_policy_still_applies() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[]);
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    );
    assert!(
        matches!(result, Err(TransferError::Configuration(_))),
        "{result:?}"
    );
}

#[test]
fn connect_failure_is_reported() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &Environment::default(),
        &mut Registered(Some(Arc::new(Refusing))),
    );
    let error = result.unwrap_err();
    assert!(matches!(error, TransferError::Transport(_)), "{error:?}");
    assert!(
        std::error::Error::source(&error)
            .is_some_and(|source| source.to_string().contains("vault is sealed"))
    );
}

#[test]
fn fetch_is_not_supported_yet() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let result = girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"]),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    );
    assert!(matches!(
        result,
        Err(TransferError::UnsupportedTransport(_))
    ));
}

// A service that fails without a listing is reported as the transport's failure.
#[cfg(unix)]
struct Failing;

#[cfg(unix)]
impl Transport for Failing {
    fn connect(&self, _request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError> {
        let child = Command::new("sh")
            .args(["-c", "exit 7"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(ConnectError::new)?;
        Ok(Box::new(ChildConnection::new(child)))
    }
}

#[cfg(unix)]
#[test]
fn failed_service_explains_missing_listing() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &Environment::default(),
        &mut Registered(Some(Arc::new(Failing))),
    );
    let error = result.unwrap_err();
    assert!(matches!(error, TransferError::Transport(_)), "{error:?}");
    assert!(error.to_string().contains("exit"), "{error}");
}
