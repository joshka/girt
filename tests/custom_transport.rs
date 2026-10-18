//! Application transports: a `mem::<path>` scheme served by `git upload-pack` through
//! `ChildConnection`, compared with `git ls-remote` on the same repository.
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::process::{Command, Stdio};
use std::sync::Arc;

use girt::Repository;
use girt::transport::{ChildConnection, ConnectError, ConnectRequest, Connection, Transport};
use girt::workflow::{Environment, TransferCallbacks, TransferError};
use pack_git::{Fixture, git};
use rstest::rstest;

// Runs the requested Git service on the path after `mem::`, with isolated configuration.
struct GitService;

impl Transport for GitService {
    fn connect(&self, request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError> {
        assert_eq!(request.scheme(), b"mem");
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
            .args([request.service().name().trim_start_matches("git-"), &path])
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

fn refs(directory: &std::path::Path, prefix: &str) -> String {
    String::from_utf8(git(
        directory,
        &["for-each-ref", "--format=%(objectname) %(refname)", prefix],
        b"",
    ))
    .unwrap()
}

#[rstest]
#[case::v0("0")]
#[case::v2("2")]
fn fetch_matches_git_fetch(#[case] version: &str) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (directory, repository) = local(
        &fixture,
        &[
            ("protocol.mem.allow", "always"),
            ("protocol.version", version),
        ],
    );
    girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"]),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();

    let expected = tempfile::tempdir().unwrap();
    git(expected.path(), &["init", "--quiet", "."], b"");
    let source = fixture.root.path().to_str().unwrap();
    git(
        expected.path(),
        &[
            "fetch",
            "--quiet",
            source,
            "refs/heads/*:refs/remotes/origin/*",
        ],
        b"",
    );
    assert_eq!(
        refs(directory.path(), "refs/remotes/origin"),
        refs(expected.path(), "refs/remotes/origin")
    );
    git(directory.path(), &["fsck", "--strict"], b"");
}

#[test]
fn clone_matches_git_clone() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    let inputs = girt::config::ConfigInputs::default().with_command(Some(
        girt::Config::parse(b"[protocol \"mem\"]\nallow = always\n").unwrap(),
    ));
    let url = format!("mem::{}", fixture.root.path().display());
    girt::workflow::clone_with(
        &url,
        scratch.path().join("girt"),
        &girt::workflow::CloneOptions::default(),
        &inputs,
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    let source = fixture.root.path().to_str().unwrap();
    git(scratch.path(), &["clone", "--quiet", source, "git"], b"");
    let girt_clone = scratch.path().join("girt");
    let git_clone = scratch.path().join("git");
    // girt's clone does not create the `origin/HEAD` alias, on any transport.
    let git_refs = refs(&git_clone, "refs").replace(
        &format!(
            "{} refs/remotes/origin/HEAD\n",
            String::from_utf8(git(&git_clone, &["rev-parse", "origin/HEAD"], b""))
                .unwrap()
                .trim()
        ),
        "",
    );
    assert_eq!(refs(&girt_clone, "refs"), git_refs);
    assert_eq!(
        git(&girt_clone, &["symbolic-ref", "HEAD"], b""),
        git(&git_clone, &["symbolic-ref", "HEAD"], b"")
    );
    git(&girt_clone, &["fsck", "--strict"], b"");
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

#[test]
fn push_publishes_like_git_push() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    let source = fixture.root.path().to_str().unwrap();
    git(scratch.path(), &["clone", "--quiet", source, "local"], b"");
    let local = scratch.path().join("local");
    git(
        &local,
        &[
            "-c",
            "user.name=Push",
            "-c",
            "user.email=push@example.com",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "pushed",
        ],
        b"",
    );
    let pushed = String::from_utf8(git(&local, &["rev-parse", "HEAD"], b""))
        .unwrap()
        .trim()
        .to_owned();
    let url = format!("mem::{source}");
    git(&local, &["remote", "set-url", "origin", &url], b"");
    git(&local, &["config", "protocol.mem.allow", "always"], b"");
    let repository = Repository::open(&local).unwrap();
    let report = girt::workflow::push(
        &repository,
        "origin",
        vec![girt::push::PushCommand::new(
            girt::refs::RefName::new("refs/heads/pushed").unwrap(),
            None,
            pushed.parse().unwrap(),
        )],
        &girt::workflow::PushOptions::default(),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    assert!(report.all_succeeded(), "{report:?}");
    let published = git(
        fixture.root.path(),
        &["rev-parse", "refs/heads/pushed"],
        b"",
    );
    assert_eq!(String::from_utf8(published).unwrap().trim(), pushed);
    git(fixture.root.path(), &["fsck", "--strict"], b"");
}

// The service exits when girt ends the session without sending; girt's own reason stands.
#[test]
fn local_push_failure_is_not_masked_by_the_service() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let main = String::from_utf8(git(fixture.root.path(), &["rev-parse", "HEAD"], b"")).unwrap();
    // The new, empty repository does not have the object it is asked to push.
    let result = girt::workflow::push(
        &repository,
        "origin",
        vec![girt::push::PushCommand::new(
            girt::refs::RefName::new("refs/heads/copy").unwrap(),
            None,
            main.trim().parse().unwrap(),
        )],
        &girt::workflow::PushOptions::default(),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    );
    assert!(matches!(result, Err(TransferError::Push(_))), "{result:?}");
}

/// Blob IDs in `rev-list --missing=print` output, sorted.
fn missing_objects(directory: &std::path::Path) -> Vec<String> {
    let listed = git(
        directory,
        &["rev-list", "--objects", "--all", "--missing=print"],
        b"",
    );
    let mut missing: Vec<String> = String::from_utf8(listed)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix('?').map(str::to_owned))
        .collect();
    missing.sort();
    missing
}

/// A blobless fetch from a server that allows filters receives commits and trees only, and
/// leaves a repository Git accepts as a partial clone of `origin`.
#[rstest]
#[case::v0_sha1("0", girt::ObjectFormat::Sha1)]
#[case::v2_sha1("2", girt::ObjectFormat::Sha1)]
#[case::v0_sha256("0", girt::ObjectFormat::Sha256)]
#[case::v2_sha256("2", girt::ObjectFormat::Sha256)]
fn blobless_fetch_leaves_a_partial_clone(
    #[case] version: &str,
    #[case] format: girt::ObjectFormat,
) {
    let fixture = Fixture::new(format, true, 3);
    git(
        fixture.root.path(),
        &["config", "uploadpack.allowFilter", "true"],
        b"",
    );
    let directory = tempfile::tempdir().unwrap();
    git(
        directory.path(),
        &["init", "--quiet", &format!("--object-format={format}"), "."],
        b"",
    );
    let url = format!("mem::{}", fixture.root.path().display());
    git(directory.path(), &["remote", "add", "origin", &url], b"");
    git(
        directory.path(),
        &["config", "protocol.mem.allow", "always"],
        b"",
    );
    git(
        directory.path(),
        &["config", "protocol.version", version],
        b"",
    );
    let repository = Repository::open(directory.path()).unwrap();

    let outcome = girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"])
            .with_filter(girt::fetch::ObjectFilter::BlobNone),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    assert!(outcome.filter_applied);

    let mut blobs: Vec<String> = fixture
        .records
        .iter()
        .filter(|(_, kind, _)| *kind == girt::ObjectKind::Blob)
        .map(|(id, _, _)| id.to_string())
        .collect();
    blobs.sort();
    assert_eq!(missing_objects(directory.path()), blobs);
    let pack = directory.path().join(".git/objects/pack");
    let promisors = std::fs::read_dir(&pack)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "promisor")
        })
        .count();
    assert_eq!(promisors, 1);
    let config = |name: &str| {
        String::from_utf8(git(directory.path(), &["config", "get", name], b"")).unwrap()
    };
    assert_eq!(config("remote.origin.promisor"), "true\n");
    assert_eq!(config("remote.origin.partialclonefilter"), "blob:none\n");
    assert_eq!(config("core.repositoryformatversion"), "1\n");
    git(directory.path(), &["fsck", "--strict"], b"");

    // A later fetch without an explicit filter keeps using the configured one.
    let repository = Repository::open(directory.path()).unwrap();
    let outcome = girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/tags/*:refs/tags/*"]),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    assert!(outcome.filter_applied);
    assert_eq!(missing_objects(directory.path()), blobs);
    git(directory.path(), &["fsck", "--strict"], b"");

    // `without_filter` overrides the configured filter, like `git fetch --no-filter`.
    let outcome = girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/tags/*:refs/tags/*"]).without_filter(),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    assert!(!outcome.filter_applied);
    assert_eq!(config("remote.origin.partialclonefilter"), "blob:none\n");
}

/// A server that does not allow filters sends every object; the fetch reports that the filter
/// was not applied and installs an ordinary pack.
#[rstest]
#[case::v0("0")]
#[case::v2("2")]
fn filter_unsupported_by_server_fetches_everything(#[case] version: &str) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (directory, repository) = local(
        &fixture,
        &[
            ("protocol.mem.allow", "always"),
            ("protocol.version", version),
        ],
    );
    let outcome = girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"])
            .with_filter(girt::fetch::ObjectFilter::BlobNone),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    assert!(!outcome.filter_applied);
    assert!(missing_objects(directory.path()).is_empty());
    let pack = directory.path().join(".git/objects/pack");
    assert!(std::fs::read_dir(&pack).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|extension| extension != "promisor")
    }));
    git(directory.path(), &["fsck", "--strict"], b"");
}

fn promisor_packs(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory.join(".git/objects/pack"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "promisor")
        })
        .count()
}

/// After a blobless fetch, `fetch_missing` asks `git upload-pack` for named blobs and installs
/// them in a second promisor pack that Git accepts. Protocol v2 allows such wants by default; v0
/// needs `uploadpack.allowAnySHA1InWant`.
#[rstest]
#[case::v0_sha1("0", girt::ObjectFormat::Sha1)]
#[case::v2_sha1("2", girt::ObjectFormat::Sha1)]
#[case::v0_sha256("0", girt::ObjectFormat::Sha256)]
#[case::v2_sha256("2", girt::ObjectFormat::Sha256)]
fn missing_blobs_fetch_from_upload_pack(#[case] version: &str, #[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 3);
    git(
        fixture.root.path(),
        &["config", "uploadpack.allowFilter", "true"],
        b"",
    );
    let directory = tempfile::tempdir().unwrap();
    git(
        directory.path(),
        &["init", "--quiet", &format!("--object-format={format}"), "."],
        b"",
    );
    let url = format!("mem::{}", fixture.root.path().display());
    git(directory.path(), &["remote", "add", "origin", &url], b"");
    for (name, value) in [
        ("protocol.mem.allow", "always"),
        ("protocol.version", version),
    ] {
        git(directory.path(), &["config", name, value], b"");
    }
    let repository = Repository::open(directory.path()).unwrap();
    girt::workflow::fetch(
        &repository,
        "origin",
        &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"])
            .with_filter(girt::fetch::ObjectFilter::BlobNone),
        &Environment::default(),
        &mut Registered(Some(Arc::new(GitService))),
    )
    .unwrap();
    let missing = missing_objects(directory.path());
    assert!(missing.len() >= 2);
    let wanted: Vec<girt::ObjectId> = missing[..2].iter().map(|id| id.parse().unwrap()).collect();

    let repository = Repository::open(directory.path()).unwrap();
    let fetch = || {
        girt::workflow::fetch_missing(
            &repository,
            "origin",
            &wanted,
            &Environment::default(),
            &mut Registered(Some(Arc::new(GitService))),
        )
    };
    if version == "0" {
        // Git's v0 upload-pack refuses wants no reference advertises.
        assert!(matches!(fetch(), Err(TransferError::Fetch(_))));
        assert_eq!(promisor_packs(directory.path()), 1);
        git(
            fixture.root.path(),
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
            b"",
        );
    }
    let report = fetch().unwrap();
    assert_eq!(report.fetched, wanted);
    assert_eq!(promisor_packs(directory.path()), 2);
    assert_eq!(missing_objects(directory.path()), missing[2..]);

    let objects = Repository::open(directory.path())
        .unwrap()
        .objects()
        .unwrap();
    for id in &wanted {
        let expected = fixture
            .records
            .iter()
            .find(|(record, _, _)| record == id)
            .unwrap();
        assert_eq!(objects.read(*id).unwrap().unwrap().data(), expected.2);
        // Git reads the blob itself; it cannot lazily fetch over the `mem::` scheme.
        assert_eq!(
            git(
                directory.path(),
                &["cat-file", "blob", &id.to_string()],
                b""
            ),
            expected.2
        );
    }
    git(directory.path(), &["fsck", "--strict"], b"");
}

/// A remote that isn't a promisor is refused before anything is sent.
#[test]
fn missing_objects_from_a_non_promisor_are_refused() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let (_directory, repository) = local(&fixture, &[("protocol.mem.allow", "always")]);
    let wanted = fixture.records[0].0;
    let result = girt::workflow::fetch_missing(
        &repository,
        "origin",
        &[wanted],
        &Environment::default(),
        &mut Registered(Some(Arc::new(Refusing))),
    );
    assert!(matches!(result, Err(TransferError::NotPromisor(name)) if name == "origin"));
}
