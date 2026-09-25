//! Captures real public operations with original disposable data, without Git or global policy.
#![cfg(feature = "tracing")]
#[path = "support/trace_capture.rs"]
mod trace_capture;

use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchError, FetchLimits};
use girt::{
    HistoryLimits, InitKind, ObjectFormat, ObjectId, ObjectKind, PackLimits, PackObject,
    PackWriteLimits, Repository,
};
use rstest::rstest;
use trace_capture::Capture;

const SECRET: &[u8] = b"R03_SECRET_content_identity_url_credential";
fn repository() -> (tempfile::TempDir, Repository) {
    let root = tempfile::Builder::new()
        .prefix("R03_SECRET_path")
        .tempdir()
        .unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    (root, repo)
}
fn pkt(bytes: &[u8]) -> Vec<u8> {
    let mut result = format!("{:04x}", bytes.len() + 4).into_bytes();
    result.extend_from_slice(bytes);
    result
}
fn transfer() -> (ObjectId, Vec<u8>) {
    let id = ObjectId::for_blob(ObjectFormat::Sha1, SECRET);
    let mut pack = Vec::new();
    girt::write_pack(
        girt::ObjectFormat::Sha1,
        &[PackObject {
            id,
            kind: ObjectKind::Blob,
            data: SECRET,
        }],
        &mut pack,
        &mut Vec::new(),
        PackWriteLimits::default(),
    )
    .unwrap();
    let mut bytes = pkt(format!("{id} refs/tags/R03_SECRET_ref\0side-band-64k\n").as_bytes());
    bytes.extend(b"0000");
    bytes.extend(pkt(b"NAK\n"));
    let mut band = vec![1];
    band.extend(pack);
    bytes.extend(pkt(&band));
    bytes.extend(b"0000");
    (id, bytes)
}
fn receive(
    mut bytes: &[u8],
    limits: FetchLimits,
    cancelled: bool,
) -> Result<girt::fetch::ReceivedFetch, FetchError> {
    girt::fetch::receive(
        &mut bytes,
        &mut Vec::new(),
        |ad| ad.refs.iter().map(|r| r.id).collect(),
        limits,
        &AtomicBool::new(cancelled),
        |_| ControlFlow::Continue(()),
    )
}

// Keep setup calls under a dispatch too: parallel tests may first register these same callsites
// while asserting captured operations. Unsubscribed setup can race that initial registration.
fn received_fixture(bytes: &[u8]) -> girt::fetch::ReceivedFetch {
    tracing::dispatcher::with_default(&Capture::default().dispatch(), || {
        receive(bytes, FetchLimits::default(), false).unwrap()
    })
}

#[test]
fn fetch_hierarchy_counts_and_sensitive_data_absence() {
    let (_, bytes) = transfer();
    let capture = Capture::default();
    let received = tracing::dispatcher::with_default(&capture.dispatch(), || {
        receive(&bytes, FetchLimits::default(), false)
    })
    .unwrap();
    assert_eq!(received.object_count(), 1);
    let import = capture.named("fetch.import");
    assert_eq!(capture.parent_name(&import), "fetch.receive");
    assert_eq!(import.fields["objects"], "1");
    assert_eq!(import.fields["outcome"], "success");
    assert_eq!(capture.named("fetch.connectivity").fields["visited"], "1");
    assert!(capture.spans().iter().all(|s| s.closed));
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

#[rstest]
#[case::cancelled(true, usize::MAX, "cancelled", "cancelled")]
#[case::limit(false, 0, "failure", "limit")]
fn fetch_failure_classes(
    #[case] cancelled: bool,
    #[case] max_wire_bytes: usize,
    #[case] outcome: &str,
    #[case] class: &str,
) {
    let (_, bytes) = transfer();
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        receive(
            &bytes,
            FetchLimits {
                max_wire_bytes,
                ..Default::default()
            },
            cancelled,
        )
    });
    assert!(result.is_err());
    let span = capture.named("fetch.receive");
    assert_eq!(span.fields["outcome"], outcome);
    assert_eq!(span.fields["failure_class"], class);
}

#[test]
fn installation_failure_retains_visible_pack_and_reports_phase() {
    let (_root, repo) = repository();
    let (_, bytes) = transfer();
    let received = received_fixture(&bytes);
    // Successful installation identifies the artifact name; removing only its index and replacing
    // it with a directory makes the second index publication fail after pack reuse succeeds.
    let installed = received
        .install(&repo, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let base = repo
        .object_dir()
        .join("pack")
        .join(format!("pack-{}", installed.checksum.unwrap()));
    std::fs::remove_file(base.with_extension("idx")).unwrap();
    std::fs::create_dir(base.with_extension("idx")).unwrap();
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        received.install(&repo, PackLimits::default(), &AtomicBool::new(false))
    });
    assert!(result.is_err());
    assert!(base.with_extension("pack").is_file());
    assert_eq!(
        capture.named("fetch.install").fields["effects"],
        "pack_visible"
    );
    assert_eq!(capture.named("fetch.install").fields["outcome"], "failure");
}

#[test]
fn history_limit_and_index_lifecycle_have_owned_spans() {
    let (_root, repo) = repository();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let capture = Capture::default();
    tracing::dispatcher::with_default(&capture.dispatch(), || {
        let error = objects
            .walk(
                &[ObjectId::for_blob(ObjectFormat::Sha1, SECRET)],
                HistoryLimits {
                    max_commits: 0,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(error, girt::HistoryError::Limit(_)));
        repo.edit_index(Default::default())
            .unwrap()
            .commit()
            .unwrap();
    });
    assert_eq!(
        capture.parent_name(&capture.named("history.graph")),
        "history.walk"
    );
    assert_eq!(
        capture.named("history.walk").fields["failure_class"],
        "limit"
    );
    assert_eq!(capture.named("index.commit").fields["outcome"], "success");
    assert!(!repo.git_dir().join("index.lock").exists());
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

#[cfg(feature = "http")]
#[path = "support/trace_http.rs"]
mod http;

#[test]
fn push_write_failure_is_uncertain_without_logging_source_text() {
    use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushError, PushLimits};
    let (_root, repo) = repository();
    let id = repo.loose_objects().write_blob(SECRET).unwrap();
    let prepared = PreparedPush::new(
        &repo.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: girt::refs::RefName::new("refs/tags/R03_SECRET_ref").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut advertisement =
        pkt(b"0000000000000000000000000000000000000000 capabilities^{}\0report-status\n");
    advertisement.extend(b"0000");
    let mut writer = FailingWriter(0);
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        girt::push::send(
            &mut advertisement.as_slice(),
            &mut writer,
            &prepared,
            &AtomicBool::new(false),
        )
    });
    assert!(matches!(result, Err(PushError::Uncertain { .. })));
    assert_eq!(writer.0, 1);
    let span = capture.named("push.send");
    assert_eq!(span.fields["effects"], "uncertain");
    assert_eq!(span.fields["failure_class"], "io");
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}
struct FailingWriter(usize);
impl std::io::Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        self.0 += 1;
        Err(std::io::Error::other("R03_SECRET_error_source"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn reference_transaction_has_prepare_and_publication_children() {
    use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
    let (_root, repo) = repository();
    let capture = Capture::default();
    let edit = RefEdit {
        name: RefName::new("refs/heads/R03_SECRET_ref").unwrap(),
        dereference: false,
        target: Some(Target::Direct(ObjectId::for_blob(
            ObjectFormat::Sha1,
            SECRET,
        ))),
        expected: Expected::Absent,
        reflog: Reflog::Preserve,
    };
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        repo.references().unwrap().transaction(&[edit])
    });
    assert_eq!(result.unwrap().len(), 1);
    assert_eq!(
        capture.parent_name(&capture.named("refs.prepare_transaction")),
        "refs.transaction"
    );
    assert_eq!(
        capture.parent_name(&capture.named("refs.publish")),
        "refs.transaction"
    );
    assert_eq!(capture.named("refs.transaction").fields["edits"], "1");
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

#[test]
fn filtered_operations_never_modify_caller_fields() {
    use tracing_subscriber::layer::SubscriberExt;
    let root = tempfile::tempdir().unwrap();
    let loose = girt::LooseObjects::new(root.path(), ObjectFormat::Sha1);
    let (_repo_dir, repo) = repository();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let capture = Capture::default();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(capture.clone())
            .with(tracing_subscriber::filter::LevelFilter::INFO),
    );
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info_span!(
            "caller",
            outcome = "caller_owned",
            failure_class = "caller_owned",
            effects = "caller_owned"
        )
        .in_scope(|| {
            assert!(
                objects
                    .peel(
                        ObjectId::null(ObjectFormat::Sha1),
                        girt::PeelLimits::default(),
                        &AtomicBool::new(false)
                    )
                    .is_err()
            );
            assert!(
                loose
                    .read_blob(ObjectId::for_blob(ObjectFormat::Sha1, SECRET), 100)
                    .is_err()
            );
        });
    });
    let caller = capture.named("caller");
    assert_eq!(caller.fields["outcome"], "caller_owned");
    assert_eq!(caller.fields["failure_class"], "caller_owned");
    assert_eq!(caller.fields["effects"], "caller_owned");
    assert_eq!(capture.spans().len(), 1);
}

#[rstest]
#[case::unknown(b"future 0\0", "unsupported")]
#[case::wrong_kind(b"tree 0\0", "wrong_kind")]
fn loose_kind_errors_remain_distinct(#[case] encoded: &[u8], #[case] class: &str) {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let loose = girt::LooseObjects::new(root.path(), ObjectFormat::Sha1);
    let id = ObjectId::for_blob(ObjectFormat::Sha1, b"");
    let hex = id.to_string();
    std::fs::create_dir(root.path().join(&hex[..2])).unwrap();
    let file = std::fs::File::create(root.path().join(&hex[..2]).join(&hex[2..])).unwrap();
    let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
    encoder.write_all(encoded).unwrap();
    encoder.finish().unwrap();
    let capture = Capture::default();
    let result =
        tracing::dispatcher::with_default(&capture.dispatch(), || loose.read_blob(id, 100));
    assert!(result.is_err());
    assert_eq!(capture.named("loose.read").fields["failure_class"], class);
}

#[test]
fn successful_push_protocol_exposes_remote_rejections_without_text() {
    use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits};
    let (_root, repo) = repository();
    let id = repo.loose_objects().write_blob(SECRET).unwrap();
    let prepared = PreparedPush::new(
        &repo.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: girt::refs::RefName::new("refs/tags/R03_SECRET_ref").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut bytes =
        pkt(b"0000000000000000000000000000000000000000 capabilities^{}\0report-status\n");
    bytes.extend(b"0000");
    bytes.extend(pkt(b"unpack ok\n"));
    bytes.extend(pkt(
        b"ng refs/tags/R03_SECRET_ref R03_SECRET_remote_reason\n",
    ));
    bytes.extend(b"0000");
    let capture = Capture::default();
    let report = tracing::dispatcher::with_default(&capture.dispatch(), || {
        girt::push::send(
            &mut bytes.as_slice(),
            &mut Vec::new(),
            &prepared,
            &AtomicBool::new(false),
        )
    })
    .unwrap();
    assert!(!report.all_succeeded());
    let span = capture.named("push.send");
    assert_eq!(span.fields["outcome"], "success");
    assert_eq!(span.fields["unpack"], "accepted");
    assert_eq!(span.fields["rejected"], "1");
    assert_eq!(span.fields["pending"], "0");
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

#[test]
fn sha256_storage_spans_include_format_and_classify_refusal() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha256,
        root.path().join("R03_SECRET_path"),
        InitKind::Bare,
    )
    .unwrap();
    let loose = repo.loose_objects();
    let capture = Capture::default();
    let id = tracing::dispatcher::with_default(&capture.dispatch(), || loose.write_blob(SECRET))
        .unwrap();
    assert_eq!(
        capture.named("loose.write").fields["object_format"],
        "sha256"
    );
    assert_eq!(capture.named("loose.write").fields["outcome"], "success");
    let capture = Capture::default();
    tracing::dispatcher::with_default(&capture.dispatch(), || {
        assert_eq!(loose.read_blob(id, 100).unwrap(), SECRET);
        repo.edit_index(Default::default())
            .unwrap()
            .abort()
            .unwrap();
    });
    assert_eq!(
        capture.named("loose.read").fields["object_format"],
        "sha256"
    );
    assert_eq!(
        capture.named("index.edit_index").fields["outcome"],
        "success"
    );
    assert!(capture.spans().iter().all(|s| s.closed));
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
    let capture = Capture::default();
    tracing::dispatcher::with_default(&capture.dispatch(), || {
        assert!(
            loose
                .write_tree(&girt::Tree::new(ObjectFormat::Sha1, vec![]).unwrap())
                .is_err()
        );
    });
    assert_eq!(
        capture.named("loose.write").fields["failure_class"],
        "unsupported"
    );
}

#[test]
fn sha1_transfer_cannot_publish_into_sha256_repository() {
    let (_, bytes) = transfer();
    let received = received_fixture(&bytes);
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha256,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let pack_dir = repo.object_dir().join("pack");
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        received.install(&repo, PackLimits::default(), &AtomicBool::new(false))
    });
    assert!(matches!(result, Err(FetchError::Unsupported(_))));
    assert_eq!(std::fs::read_dir(pack_dir).unwrap().count(), 0);
    assert_eq!(
        capture.named("fetch.install").fields["failure_class"],
        "unsupported"
    );
    assert!(
        !capture
            .named("fetch.install")
            .fields
            .contains_key("effects")
    );
}

#[test]
fn peel_span_owns_read_children_and_redacts_payloads() {
    let (_root, repo) = repository();
    let id = repo.loose_objects().write_blob(SECRET).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        objects.peel(id, girt::PeelLimits::default(), &AtomicBool::new(false))
    })
    .unwrap();
    assert_eq!(result.target, id);
    let peel = capture.named("objects.peel");
    assert_eq!(peel.fields["outcome"], "success");
    assert_eq!(
        capture.parent_name(&capture.named("objects.read")),
        "objects.peel"
    );
    assert!(capture.spans().iter().all(|s| s.closed));
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

#[rstest]
#[case::missing(false, "failure", "missing")]
#[case::cancelled(true, "cancelled", "cancelled")]
fn peel_failure_classification(#[case] cancel: bool, #[case] outcome: &str, #[case] class: &str) {
    let (_root, repo) = repository();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        objects.peel(
            ObjectId::for_blob(ObjectFormat::Sha1, SECRET),
            girt::PeelLimits::default(),
            &AtomicBool::new(cancel),
        )
    });
    assert!(result.is_err());
    let span = capture.named("objects.peel");
    assert_eq!(span.fields["outcome"], outcome);
    assert_eq!(span.fields["failure_class"], class);
}

#[test]
fn config_resolution_span_is_categorical_and_redacted() {
    let root = tempfile::Builder::new()
        .prefix("R03_SECRET_path")
        .tempdir()
        .unwrap();
    let path = root.path().join("config");
    std::fs::write(
        &path,
        b"[remote \"R03_SECRET_name\"]\nurl=R03_SECRET_url\n[include]\npath=config\n",
    )
    .unwrap();
    let inputs = girt::config::ConfigInputs {
        files: vec![girt::config::ConfigFile {
            path,
            scope: girt::config::ConfigScope::Local,
            optional: false,
        }],
        ..Default::default()
    };
    let capture = Capture::default();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        let parent = tracing::info_span!("config.owner");
        parent.in_scope(|| girt::Config::resolve(&inputs))
    });
    assert!(result.is_err());
    let span = capture.named("config.resolve");
    assert_eq!(capture.parent_name(&span), "config.owner");
    assert_eq!(span.fields["outcome"], "failure");
    assert_eq!(span.fields["failure_class"], "cycle");
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.spans().iter().all(|s| s.closed));
    assert!(capture.events().is_empty());
}

#[test]
fn filtered_config_resolution_keeps_parent_completion_fields() {
    use tracing_subscriber::layer::SubscriberExt;
    let capture = Capture::default();
    let dispatch =
        tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()).with(
            tracing_subscriber::filter::filter_fn(|metadata| metadata.name() != "config.resolve"),
        ));
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info_span!(
            "config.parent",
            outcome = "parent_owned",
            failure_class = "parent_owned"
        )
        .in_scope(|| girt::Config::resolve(&girt::config::ConfigInputs::default()).unwrap());
    });
    assert_eq!(
        capture.named("config.parent").fields["outcome"],
        "parent_owned"
    );
    assert_eq!(
        capture.named("config.parent").fields["failure_class"],
        "parent_owned"
    );
}
