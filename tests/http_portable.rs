//! Real Git smart HTTP without girt reference storage or Unix hooks/process adapters.
//! Python and Git serve disposable loopback repositories; all network calls have deadlines.
#![cfg(feature = "http")]
#[path = "support/http_git.rs"]
mod http_git;
#[path = "support/pack_git.rs"]
mod pack_git;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::{self, Advertisement, FetchError, FetchLimits, KnownHistory};
use girt::push::{self, ForcePolicy, PreparedPush, PushCommand, PushLimits, Status};
use girt::refs::RefName;
use girt::remote::{
    CredentialContext, CredentialHelper, CredentialProgram, CredentialSession, Prompt,
    ProtocolEnvironment, Remote,
};
use girt::transport::TransportControl;
use girt::transport::http::{HttpEnvironment, HttpError, HttpRemote, HttpSettings};
use girt::{Config, InitKind, ObjectId, PackLimits, ReadLimits, Repository};
use http_git::Server;
use pack_git::{Fixture, git};
use rstest::rstest;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(20)),
    }
}

fn all(advertisement: &Advertisement) -> Vec<ObjectId> {
    advertisement
        .refs
        .iter()
        .filter(|r| !r.peeled)
        .map(|r| r.id)
        .collect()
}

#[test]
fn configured_connection_retries_challenged_discovery_at_its_bound_origin() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), "", "Basic dTpw", None);
    let config = Config::parse(
        format!(
            "[remote \"r\"]\nurl = {}\n[credential]\nusername = u\n",
            server.url
        )
        .as_bytes(),
    )
    .unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let settings =
        HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).unwrap();
    let context = CredentialContext {
        protocol: b"http".to_vec(),
        host: server.url.split('/').nth(2).unwrap().as_bytes().to_vec(),
        path: Some(b"repo".to_vec()),
        use_http_path: false,
    };
    let cancel = AtomicBool::new(false);
    let mut prompt = |field| (field == Prompt::Password).then(|| b"p".to_vec());
    let session =
        CredentialSession::fill(&config, context, &[], Some(&mut prompt), &cancel, None).unwrap();
    let remote = HttpRemote::configured(settings)
        .unwrap()
        .with_credentials(session)
        .unwrap();
    let downloaded = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    assert!(
        !downloaded
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .wants()
            .is_empty()
    );
    assert_eq!(server.requests(), ["GET", "GET", "POST"]);
}

#[rstest]
#[case::accepted("", "Basic dTpw", &["get", "store"], &["GET", "GET", "POST"], true)]
#[case::rejected("", "Basic b3RoZXI6cA==", &["get", "erase"], &["GET", "GET"], false)]
#[case::unsupported("bearer-challenge", "", &["get"], &["GET"], false)]
fn helper_notification_follows_http_authentication_outcome(
    #[case] fault: &str,
    #[case] expected_authorization: &str,
    #[case] expected_actions: &[&str],
    #[case] expected_requests: &[&str],
    #[case] success: bool,
) {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), fault, expected_authorization, None);
    let config = Config::parse(b"[credential]\nhelper = fixture\n").unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let helper = CredentialHelper {
        configured_name: b"fixture".to_vec(),
        program: CredentialProgram {
            executable: if cfg!(windows) { "python" } else { "python3" }.into(),
            arguments: vec![
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/http/credential.py"
                )
                .into(),
                log.path().as_os_str().to_owned(),
            ],
            inherit_environment: true,
            environment: Vec::new(),
        },
    };
    let context = CredentialContext {
        protocol: b"http".to_vec(),
        host: server.url.split('/').nth(2).unwrap().as_bytes().to_vec(),
        path: Some(b"repo".to_vec()),
        use_http_path: false,
    };
    let cancel = AtomicBool::new(false);
    let session =
        CredentialSession::fill(&config, context, &[helper], None, &cancel, None).unwrap();
    let remote = HttpRemote::new(&server.url, &[], &[])
        .unwrap()
        .with_credentials(session)
        .unwrap();
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control(&cancel),
    ));
    assert_eq!(result.is_ok(), success);
    assert_eq!(
        std::fs::read_to_string(log.path())
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected_actions
    );
    assert_eq!(server.requests(), expected_requests);
}

#[test]
fn insecure_tls_requires_application_approval_before_connecting() {
    let config = Config::parse(
        b"[remote \"r\"]\nurl = https://example.invalid/repo\n[http]\nsslVerify = false\n",
    )
    .unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    assert!(matches!(
        HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()),
        Err(HttpError::Configuration(
            "insecure TLS requires application approval"
        ))
    ));
}

#[test]
fn configured_connection_follows_initial_same_origin_redirect_for_rpc() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), "same-origin-redirect", "", None);
    let config =
        Config::parse(format!("[remote \"r\"]\nurl = {}\n", server.url).as_bytes()).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let settings =
        HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).unwrap();
    let remote = HttpRemote::configured(settings).unwrap();
    let cancel = AtomicBool::new(false);
    let downloaded = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    assert!(
        !downloaded
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .wants()
            .is_empty()
    );
    assert_eq!(server.requests(), ["GET", "GET", "POST"]);
}

#[test]
fn configured_connection_uses_approved_proxy_basic_authentication() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), "proxy-auth", "", None);
    let config = Config::parse(b"[remote \"r\"]\nurl = http://example.invalid/repo\n").unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let proxy = server.url.trim_end_matches("/repo").to_owned();
    let environment = HttpEnvironment {
        proxy: Some(proxy),
        proxy_credentials: Some(("pu".to_owned(), "pp".to_owned())),
        ..HttpEnvironment::default()
    };
    let settings = HttpSettings::resolve(&config, &destination, &environment).unwrap();
    let remote = HttpRemote::configured(settings).unwrap();
    let cancel = AtomicBool::new(false);
    let downloaded = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    assert!(
        !downloaded
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .wants()
            .is_empty()
    );
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[test]
fn downloads_validates_installs_and_reuses_known_objects() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let source_index = std::fs::read(&source.index_path).unwrap();
    let server = Server::new(source.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    let downloaded = rt
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    let received = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            downloaded.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
        .unwrap()
    });
    let root = tempfile::tempdir().unwrap();
    let destination = Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    let objects = destination.objects(PackLimits::default()).unwrap();
    let expected = &source.records[0];
    let actual = objects
        .read(expected.0, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(actual.kind(), expected.1);
    assert_eq!(actual.data(), expected.2);
    assert_eq!(
        git(
            &root.path().join("repo"),
            &["cat-file", "blob", &expected.0.to_string()],
            b""
        ),
        expected.2
    );
    let known =
        KnownHistory::new(&objects, received.wants(), FetchLimits::default(), &cancel).unwrap();
    let downloaded = rt
        .block_on(fetch::receive_http(
            &remote,
            all,
            Some(Arc::new(known)),
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    let received = downloaded
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap();
    let installed = received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    assert!(installed.checksum.is_none());
    assert_eq!(server.requests(), ["GET", "POST", "GET"]);
    assert!(
        objects
            .read(source.ordinary, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(
        objects
            .read(source.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(std::fs::read(&source.index_path).unwrap(), source_index);
}

#[test]
fn pushes_objects_and_git_publishes_the_remote_ref() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let root = tempfile::tempdir().unwrap();
    Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    git(
        &root.path().join("repo"),
        &["config", "http.receivepack", "true"],
        b"",
    );
    let server = Server::new(&root.path().join("repo"), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let bytes = git(source.root.path(), &["rev-parse", "main"], b"");
    let tip: ObjectId = std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap();
    let prepared = PreparedPush::new(
        &source.repo.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: None,
            new: tip,
            force: ForcePolicy::FastForwardOnly,
        }],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = runtime()
        .block_on(push::send_http(&remote, prepared, control(&cancel)))
        .unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(
        git(&root.path().join("repo"), &["rev-parse", "main"], b""),
        bytes
    );
    git(&root.path().join("repo"), &["fsck", "--strict"], b"");
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[rstest]
#[case::unauthorized("", &[], 401)]
#[case::server_failure("failure", &[("Authorization", "secret")], 503)]
#[case::redirect("redirect", &[("Authorization", "secret")], 302)]
fn rejects_http_status_without_retry(
    #[case] fault: &str,
    #[case] headers: &[(&str, &str)],
    #[case] expected: u16,
) {
    let root = tempfile::tempdir().unwrap();
    Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let server = Server::new(&root.path().join("repo"), fault, "secret", None);
    let remote = HttpRemote::new(&server.url, headers, &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control(&cancel),
    ));
    assert!(
        matches!(result, Err(FetchError::Http(HttpError::Status(status))) if status == expected)
    );
    assert_eq!(server.requests(), ["GET"]);
}

#[test]
fn stalled_discovery_observes_deadline() {
    let root = tempfile::tempdir().unwrap();
    Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let server = Server::new(&root.path().join("repo"), "stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    };
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control,
    ));
    assert!(matches!(result, Err(FetchError::Deadline)));
}

#[test]
fn truncated_rpc_never_produces_installable_objects() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let server = Server::new(source.root.path(), "post-truncate", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control(&cancel),
    ));
    assert!(matches!(result, Err(FetchError::Http(HttpError::Network))));
    assert_eq!(server.requests(), ["GET", "POST"]);
}
