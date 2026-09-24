use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use rstest::rstest;

use super::*;
use crate::refs::RefName;
use crate::{
    Commit, CommitFields, EntryMode, ObjectId, ObjectKind, PackLimits, Repository, Signature, Tag,
    TagFields, Tree, TreeEntry,
};

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("objects")).unwrap();
        std::fs::create_dir(root.path().join("refs")).unwrap();
        std::fs::write(root.path().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(root.path().join("config"), b"[core]\nbare=true\n").unwrap();
        let repo = Repository::open(root.path()).unwrap();
        Self { _root: root, repo }
    }
    fn blob(&self) -> ObjectId {
        self.repo
            .loose_objects()
            .unwrap()
            .write_blob(b"payload")
            .unwrap()
    }
    fn tree(&self, id: ObjectId, mode: EntryMode) -> ObjectId {
        let tree = Tree::new(vec![TreeEntry {
            mode,
            id,
            name: b"entry".to_vec(),
        }])
        .unwrap();
        self.repo
            .loose_objects()
            .unwrap()
            .write_tree(&tree)
            .unwrap()
    }
    fn commit(&self, tree: ObjectId, parents: Vec<ObjectId>) -> ObjectId {
        let signature = Signature {
            name: b"A".to_vec(),
            email: b"a@example.com".to_vec(),
            seconds: 0,
            offset_minutes: 0,
        };
        let commit = Commit::new(CommitFields {
            tree,
            parents,
            author: signature.clone(),
            committer: signature,
            extra_headers: vec![],
            message: vec![],
        })
        .unwrap();
        self.repo
            .loose_objects()
            .unwrap()
            .write_commit(&commit)
            .unwrap()
    }
    fn tag(&self, target: ObjectId, kind: ObjectKind) -> ObjectId {
        let tag = Tag::new(TagFields {
            target,
            target_kind: kind,
            name: b"test".to_vec(),
            tagger: None,
            extra_headers: vec![],
            message: vec![],
        })
        .unwrap();
        self.repo.loose_objects().unwrap().write_tag(&tag).unwrap()
    }
    fn prepare(
        &self,
        commands: Vec<PushCommand>,
        limits: PushLimits,
    ) -> Result<PreparedPush, PushFailure> {
        PreparedPush::new(
            &self.repo.objects(PackLimits::default()).unwrap(),
            commands,
            limits,
            &AtomicBool::new(false),
        )
    }
}
fn command(name: &str, expected: Option<ObjectId>, new: ObjectId) -> PushCommand {
    PushCommand {
        name: RefName::new(name).unwrap(),
        expected,
        new,
        force: ForcePolicy::FastForwardOnly,
    }
}
fn tag_command(id: ObjectId) -> PushCommand {
    command("refs/tags/test", None, id)
}
fn prepared(limits: PushLimits) -> PreparedPush {
    let f = Fixture::new();
    f.prepare(vec![tag_command(f.blob())], limits).unwrap()
}
fn pkt(bytes: &[u8]) -> Vec<u8> {
    let mut result = format!("{:04x}", bytes.len() + 4).into_bytes();
    result.extend(bytes);
    result
}
fn advertisement(caps: &str) -> Vec<u8> {
    let mut result = pkt(format!(
        "{} capabilities^{{}}\0{caps}\n",
        ObjectId::from_bytes([0; 20])
    )
    .as_bytes());
    result.extend(b"0000");
    result
}
fn response(lines: &[&[u8]]) -> Vec<u8> {
    let mut result = advertisement("report-status atomic side-band-64k object-format=sha1");
    for line in lines {
        result.extend(pkt(line));
    }
    result.extend(b"0000");
    result
}
fn run(bytes: &[u8], prepared: &PreparedPush) -> Result<PushReport, PushError> {
    send(
        &mut &*bytes,
        &mut Vec::new(),
        prepared,
        &AtomicBool::new(false),
    )
}

#[test]
fn selects_complete_graph_and_skips_external_gitlinks() {
    let f = Fixture::new();
    let blob = f.blob();
    let subtree = f.tree(blob, EntryMode::Blob);
    let tree = f.tree(subtree, EntryMode::Tree);
    let external = f.tree(ObjectId::for_blob(b"external"), EntryMode::Gitlink);
    let parent = f.commit(external, vec![]);
    let commit = f.commit(tree, vec![parent, parent]);
    let tag = f.tag(commit, ObjectKind::Commit);
    let nested = f.tag(tag, ObjectKind::Tag);
    let push = f
        .prepare(
            vec![
                tag_command(nested),
                command("refs/heads/main", Some(parent), commit),
            ],
            PushLimits::default(),
        )
        .unwrap();
    assert_eq!(push.object_count(), 8);
    assert!(push.pack_bytes() > 32);
}
#[rstest]
#[case::commit_tree(ObjectKind::Commit)]
#[case::tree_child(ObjectKind::Tree)]
#[case::tag_target(ObjectKind::Tag)]
fn rejects_missing_or_mistyped_edges(#[case] kind: ObjectKind) {
    let f = Fixture::new();
    let blob = f.blob();
    let id = bad_edge(&f, blob, kind);
    assert!(matches!(
        f.prepare(vec![tag_command(id)], PushLimits::default()),
        Err(PushFailure::Kind(_))
    ));
    let missing = bad_edge(&f, ObjectId::for_blob(b"absent"), kind);
    assert!(matches!(
        f.prepare(vec![tag_command(missing)], PushLimits::default()),
        Err(PushFailure::Missing(_))
    ));
}
fn bad_edge(f: &Fixture, id: ObjectId, kind: ObjectKind) -> ObjectId {
    match kind {
        ObjectKind::Commit => f.commit(id, vec![]),
        ObjectKind::Tree => f.tree(id, EntryMode::Tree),
        ObjectKind::Tag => f.tag(id, ObjectKind::Commit),
        _ => unreachable!(),
    }
}
#[test]
fn branch_requires_commit_even_with_force() {
    let f = Fixture::new();
    let mut c = command("refs/heads/main", None, f.blob());
    c.force = ForcePolicy::Allow;
    assert!(matches!(
        f.prepare(vec![c], PushLimits::default()),
        Err(PushFailure::Kind(_))
    ));
}
#[test]
fn missing_parent_is_not_assumed_to_exist_remotely() {
    let f = Fixture::new();
    let tree = f.tree(f.blob(), EntryMode::Blob);
    let id = f.commit(tree, vec![ObjectId::for_blob(b"missing")]);
    assert!(matches!(
        f.prepare(vec![tag_command(id)], PushLimits::default()),
        Err(PushFailure::Missing(_))
    ));
}
#[test]
fn typed_edge_to_previously_selected_root_is_validated() {
    let f = Fixture::new();
    let blob = f.blob();
    let tag = f.tag(blob, ObjectKind::Tree);
    assert!(matches!(
        f.prepare(
            vec![tag_command(blob), command("refs/tags/other", None, tag)],
            PushLimits::default()
        ),
        Err(PushFailure::Kind(_))
    ));
}
#[rstest]
#[case::branch("refs/heads/main")]
#[case::tag("refs/tags/test")]
fn replacement_requires_explicit_force(#[case] name: &str) {
    let f = Fixture::new();
    let tree = f.tree(f.blob(), EntryMode::Blob);
    let tip = f.commit(tree, vec![]);
    let mut c = command(name, Some(ObjectId::for_blob(b"unrelated")), tip);
    assert!(matches!(
        f.prepare(vec![c.clone()], PushLimits::default()),
        Err(PushFailure::WouldForce(_))
    ));
    c.force = ForcePolicy::Allow;
    assert!(f.prepare(vec![c], PushLimits::default()).is_ok());
}
#[test]
fn proves_ancestry_along_second_merge_parent() {
    let f = Fixture::new();
    let tree = f.tree(f.blob(), EntryMode::Blob);
    let a = f.commit(tree, vec![]);
    let b = f.commit(tree, vec![a]);
    let merge = f.commit(tree, vec![a, b]);
    assert!(
        f.prepare(
            vec![command("refs/heads/main", Some(b), merge)],
            PushLimits::default()
        )
        .is_ok()
    );
}
#[rstest]
#[case::count(0, 7, 7, false)]
#[case::payload(1, 6, 7, false)]
#[case::object(1, 7, 6, false)]
#[case::exact(1, 7, 7, true)]
fn bounds_selected_objects_and_bytes(
    #[case] count: u32,
    #[case] total: u64,
    #[case] object: u64,
    #[case] ok: bool,
) {
    let f = Fixture::new();
    let mut limits = PushLimits::default();
    limits.pack.max_objects = count;
    limits.pack.max_input_bytes = total;
    limits.pack.max_object_bytes = object;
    assert_eq!(f.prepare(vec![tag_command(f.blob())], limits).is_ok(), ok);
}
#[test]
fn bounds_edges_and_ancestry_proof() {
    let f = Fixture::new();
    let tree = f.tree(f.blob(), EntryMode::Blob);
    let a = f.commit(tree, vec![]);
    let b = f.commit(tree, vec![a]);
    let c = command("refs/heads/main", Some(a), b);
    let limits = PushLimits {
        max_edges: 3,
        ..PushLimits::default()
    };
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Limit("reachable edges"))
    ));
    let limits = PushLimits {
        max_ancestry_steps: 1,
        ..PushLimits::default()
    };
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Limit("ancestry steps"))
    ));
    let limits = PushLimits {
        max_edges: 4,
        max_ancestry_steps: 3,
        ..PushLimits::default()
    };
    assert!(f.prepare(vec![c], limits).is_ok());
}
#[rstest]
#[case::head("HEAD")]
#[case::other("refs/custom/value")]
fn rejects_unsupported_namespaces(#[case] name: &str) {
    let f = Fixture::new();
    assert!(matches!(
        f.prepare(vec![command(name, None, f.blob())], PushLimits::default()),
        Err(PushFailure::Unsupported(_))
    ));
}
#[test]
fn rejects_duplicate_destinations_and_zero_ids() {
    let f = Fixture::new();
    let c = tag_command(f.blob());
    assert!(matches!(
        f.prepare(vec![c.clone(), c], PushLimits::default()),
        Err(PushFailure::Command(_))
    ));
    assert!(matches!(
        f.prepare(
            vec![tag_command(ObjectId::from_bytes([0; 20]))],
            PushLimits::default()
        ),
        Err(PushFailure::Command(_))
    ));
    assert!(matches!(
        f.prepare(
            vec![command(
                "refs/tags/test",
                Some(ObjectId::from_bytes([0; 20])),
                f.blob()
            )],
            PushLimits::default()
        ),
        Err(PushFailure::Command(_))
    ));
}
#[test]
fn sends_only_report_status_then_raw_pack_and_retains_rejections() {
    let p = prepared(PushLimits::default());
    let mut sent = vec![];
    let bytes = response(&[b"unpack ok\n", b"ng refs/tags/test hook declined\n"]);
    let report = send(
        &mut bytes.as_slice(),
        &mut sent,
        &p,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!report.all_succeeded());
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(b"hook declined".to_vec()))
    );
    assert_eq!(&sent[..p.request.len()], p.request);
    assert_eq!(&sent[p.request.len()..p.request.len() + 4], b"PACK");
    assert!(sent.windows(14).any(|w| w == b"\0report-status"));
    assert!(!sent.windows(6).any(|w| w == b"atomic"));
}
#[test]
fn preserves_unpack_failure_and_binary_reason() {
    let bytes = response(&[b"unpack bad pack\n", b"ng refs/tags/test reason \xff\n"]);
    let report = run(&bytes, &prepared(PushLimits::default())).unwrap();
    assert_eq!(report.unpack, Some(Status::Rejected(b"bad pack".to_vec())));
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(b"reason \xff".to_vec()))
    );
}
#[rstest]
#[case::no_status("ofs-delta")]
#[case::v2_only("report-status-v2")]
#[case::sha256("report-status object-format=sha256")]
fn refuses_unsupported_capabilities_before_writing(#[case] caps: &str) {
    let mut written = vec![];
    let result = send(
        &mut advertisement(caps).as_slice(),
        &mut written,
        &prepared(PushLimits::default()),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(PushError::NotSent(PushFailure::Unsupported(_)))
    ));
    assert!(written.is_empty());
}
#[rstest]
#[case::version(b"000eversion 2\n0000")]
#[case::header(b"zzzz")]
#[case::length(b"0001")]
#[case::truncated(b"0008abc")]
#[case::missing(b"0000")]
fn invalid_advertisement_never_sends_commands(#[case] bytes: &[u8]) {
    let mut written = vec![];
    assert!(matches!(
        send(
            &mut &*bytes,
            &mut written,
            &prepared(PushLimits::default()),
            &AtomicBool::new(false)
        ),
        Err(PushError::NotSent(_))
    ));
    assert!(written.is_empty());
}
#[test]
fn stale_expectation_never_sends_commands() {
    let p = prepared(PushLimits::default());
    let mut bytes = pkt(format!("{} refs/tags/test\0report-status", p.commands[0].new).as_bytes());
    bytes.extend(b"0000");
    assert!(matches!(
        run(&bytes, &p),
        Err(PushError::NotSent(PushFailure::Stale { .. }))
    ));
}
#[rstest]
#[case::missing_unpack(&[])]
#[case::missing_ref(&[b"unpack ok".as_slice()])]
#[case::wrong_ref(&[b"unpack ok".as_slice(), b"ok refs/tags/other".as_slice()])]
#[case::duplicate(&[b"unpack ok".as_slice(), b"ok refs/tags/test".as_slice(), b"ng refs/tags/test duplicate".as_slice()])]
#[case::v2_option(&[b"unpack ok".as_slice(), b"ok refs/tags/test".as_slice(), b"option forced-update".as_slice()])]
#[case::contradiction(&[b"unpack bad".as_slice(), b"ok refs/tags/test".as_slice()])]
#[case::empty_unpack(&[b"unpack ".as_slice()])]
#[case::empty_reason(&[b"unpack ok".as_slice(), b"ng refs/tags/test ".as_slice()])]
#[case::remote_err(&[b"ERR failure".as_slice()])]
fn malformed_status_is_uncertain(#[case] lines: &[&[u8]]) {
    assert!(matches!(
        run(&response(lines), &prepared(PushLimits::default())),
        Err(PushError::Uncertain { .. })
    ));
}
#[test]
fn truncated_status_preserves_valid_prefix() {
    let mut bytes = response(&[b"unpack ok", b"ok refs/tags/test"]);
    bytes.truncate(bytes.len() - 2);
    let Err(PushError::Uncertain { report, .. }) = run(&bytes, &prepared(PushLimits::default()))
    else {
        panic!("expected uncertain");
    };
    assert_eq!(report.unpack, Some(Status::Ok));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
}
#[test]
fn rejects_trailing_bytes_after_complete_report() {
    let mut bytes = response(&[b"unpack ok", b"ok refs/tags/test"]);
    bytes.extend(b"extra");
    assert!(matches!(
        run(&bytes, &prepared(PushLimits::default())),
        Err(PushError::Uncertain { .. })
    ));
}
#[test]
fn bounds_advertisement_and_status_before_allocating_packets() {
    let p = prepared(PushLimits {
        max_advertisement_bytes: 4,
        ..PushLimits::default()
    });
    assert!(matches!(
        run(&advertisement("report-status"), &p),
        Err(PushError::NotSent(PushFailure::Limit(_)))
    ));
    let p = prepared(PushLimits {
        max_status_bytes: 4,
        ..PushLimits::default()
    });
    assert!(matches!(
        run(&response(&[b"unpack ok"]), &p),
        Err(PushError::Uncertain {
            cause: PushFailure::Limit(_),
            ..
        })
    ));
}
#[test]
fn empty_push_only_flushes() {
    let f = Fixture::new();
    let p = f.prepare(vec![], PushLimits::default()).unwrap();
    let mut out = vec![];
    assert!(
        send(
            &mut advertisement("").as_slice(),
            &mut out,
            &p,
            &AtomicBool::new(false)
        )
        .unwrap()
        .all_succeeded()
    );
    assert_eq!(out, b"0000");
    assert_eq!(p.pack_bytes(), 0);
}
struct Interrupted;
impl Read for Interrupted {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::Interrupted.into())
    }
}
impl Write for Interrupted {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::Interrupted.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn interrupted_io_is_not_retried_and_is_classified_by_phase() {
    let p = prepared(PushLimits::default());
    assert!(
        matches!(send(&mut Interrupted, &mut vec![], &p, &AtomicBool::new(false)), Err(PushError::NotSent(PushFailure::Io(e))) if e.kind() == io::ErrorKind::Interrupted)
    );
    assert!(
        matches!(send(&mut advertisement("report-status").as_slice(), &mut Interrupted, &p, &AtomicBool::new(false)), Err(PushError::Uncertain { cause: PushFailure::Io(e), .. }) if e.kind() == io::ErrorKind::Interrupted)
    );
}
struct CancelWriter<'a>(&'a AtomicBool);
impl Write for CancelWriter<'_> {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.0.store(true, Ordering::Relaxed);
        Ok(1)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn cancellation_before_and_during_transmission_has_distinct_outcomes() {
    let p = prepared(PushLimits::default());
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        send(
            &mut advertisement("report-status").as_slice(),
            &mut vec![],
            &p,
            &cancel
        ),
        Err(PushError::NotSent(PushFailure::Cancelled))
    ));
    cancel.store(false, Ordering::Relaxed);
    assert!(matches!(
        send(
            &mut advertisement("report-status").as_slice(),
            &mut CancelWriter(&cancel),
            &p,
            &cancel
        ),
        Err(PushError::Uncertain {
            cause: PushFailure::Cancelled,
            ..
        })
    ));
}

#[test]
fn preparation_cancellation_changes_no_storage() {
    let f = Fixture::new();
    let objects = f.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        PreparedPush::new(
            &objects,
            vec![tag_command(ObjectId::for_blob(b"absent"))],
            PushLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(PushFailure::Cancelled)
    ));
    assert_eq!(std::fs::read_dir(f.repo.object_dir()).unwrap().count(), 0);
}
#[test]
fn bounds_command_count_and_encoded_bytes_exactly() {
    let f = Fixture::new();
    let c = tag_command(f.blob());
    let limits = PushLimits {
        max_commands: 0,
        ..PushLimits::default()
    };
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Limit("commands"))
    ));
    let p = f.prepare(vec![c.clone()], PushLimits::default()).unwrap();
    let limits = PushLimits {
        max_command_bytes: p.request.len() - 1,
        ..PushLimits::default()
    };
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Limit("command bytes"))
    ));
    let limits = PushLimits {
        max_command_bytes: p.request.len(),
        ..PushLimits::default()
    };
    assert!(f.prepare(vec![c], limits).is_ok());
}
#[test]
fn bounds_generated_pack_and_index() {
    let f = Fixture::new();
    let c = tag_command(f.blob());
    let p = f.prepare(vec![c.clone()], PushLimits::default()).unwrap();
    let mut limits = PushLimits::default();
    limits.pack.max_pack_bytes = p.pack_bytes() as u64 - 1;
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Pack(_))
    ));
    limits.pack.max_pack_bytes += 1;
    limits.pack.max_index_bytes = 1099;
    assert!(matches!(
        f.prepare(vec![c.clone()], limits),
        Err(PushFailure::Pack(_))
    ));
    limits.pack.max_index_bytes = 1100; // 1072 fixed bytes + 28 per ordinary offset entry.
    assert!(f.prepare(vec![c], limits).is_ok());
}
#[test]
fn exact_wire_budgets_allow_complete_report() {
    let bytes = response(&[b"unpack ok", b"ok refs/tags/test"]);
    let adv = advertisement("report-status atomic side-band-64k object-format=sha1").len();
    let p = prepared(PushLimits {
        max_advertisement_bytes: adv,
        max_status_bytes: bytes.len() - adv,
        ..PushLimits::default()
    });
    assert!(run(&bytes, &p).unwrap().all_succeeded());
}
#[test]
fn bounds_advertised_refs_and_accepts_unselected_have_entries() {
    let p = prepared(PushLimits {
        max_refs: 0,
        ..PushLimits::default()
    });
    let mut bytes = pkt(format!("{} .have\0report-status", p.commands[0].new).as_bytes());
    bytes.extend(b"0000");
    assert!(matches!(
        run(&bytes, &p),
        Err(PushError::NotSent(PushFailure::Limit("advertised refs")))
    ));
    bytes.extend(pkt(b"unpack ok"));
    bytes.extend(pkt(b"ok refs/tags/test"));
    bytes.extend(b"0000");
    assert!(
        run(&bytes, &prepared(PushLimits::default()))
            .unwrap()
            .all_succeeded()
    );
}
#[test]
fn returns_partial_results_in_command_order_even_if_server_reorders() {
    let f = Fixture::new();
    let id = f.blob();
    let p = f
        .prepare(
            vec![tag_command(id), command("refs/tags/other", None, id)],
            PushLimits::default(),
        )
        .unwrap();
    let bytes = response(&[
        b"unpack ok",
        b"ng refs/tags/other rejected",
        b"ok refs/tags/test",
    ]);
    let report = run(&bytes, &p).unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(
        report.refs[1].status,
        Some(Status::Rejected(b"rejected".to_vec()))
    );
}
struct ShortWriter(Vec<u8>);
impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.push(bytes[0]);
        Ok(1)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn short_writes_preserve_full_request_and_pack() {
    let p = prepared(PushLimits::default());
    let bytes = response(&[b"unpack ok", b"ok refs/tags/test"]);
    let mut writer = ShortWriter(vec![]);
    assert!(
        send(
            &mut bytes.as_slice(),
            &mut writer,
            &p,
            &AtomicBool::new(false)
        )
        .unwrap()
        .all_succeeded()
    );
    assert_eq!(writer.0, [p.request.as_slice(), p.pack.as_slice()].concat());
}
struct FailingWriter {
    bytes: usize,
    fail_flush: bool,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = bytes.len().min(self.bytes);
        self.bytes -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}
#[rstest]
#[case::pack(200, false)]
#[case::flush(usize::MAX, true)]
fn later_output_failures_are_uncertain(#[case] bytes: usize, #[case] fail_flush: bool) {
    let p = prepared(PushLimits::default());
    let mut writer = FailingWriter { bytes, fail_flush };
    assert!(matches!(
        send(
            &mut advertisement("report-status").as_slice(),
            &mut writer,
            &p,
            &AtomicBool::new(false)
        ),
        Err(PushError::Uncertain { .. })
    ));
}
#[test]
fn interrupted_read_during_report_is_uncertain() {
    let p = prepared(PushLimits::default());
    let ad = advertisement("report-status");
    let mut reader = ad.as_slice().chain(Interrupted);
    assert!(
        matches!(send(&mut reader, &mut vec![], &p, &AtomicBool::new(false)), Err(PushError::Uncertain { cause: PushFailure::Io(e), .. }) if e.kind() == io::ErrorKind::Interrupted)
    );
}
#[rstest]
#[case::tree(ObjectKind::Tree)]
#[case::commit(ObjectKind::Commit)]
#[case::tag(ObjectKind::Tag)]
fn malformed_reachable_payload_is_rejected(#[case] kind: ObjectKind) {
    let f = Fixture::new();
    let id = raw_object(&f, kind, b"invalid");
    assert!(
        f.prepare(vec![tag_command(id)], PushLimits::default())
            .is_err()
    );
}
fn raw_object(f: &Fixture, kind: ObjectKind, data: &[u8]) -> ObjectId {
    let id = ObjectId::for_object(kind.as_str(), data);
    let hex = id.to_string();
    let directory = f.repo.object_dir().join(&hex[..2]);
    std::fs::create_dir_all(&directory).unwrap();
    let mut encoder = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
    encoder
        .write_all(format!("{} {}\0", kind.as_str(), data.len()).as_bytes())
        .unwrap();
    encoder.write_all(data).unwrap();
    std::fs::write(directory.join(&hex[2..]), encoder.finish().unwrap()).unwrap();
    id
}

#[test]
fn ancestry_budget_charges_duplicate_parent_edges() {
    let f = Fixture::new();
    let tree = f.tree(f.blob(), EntryMode::Blob);
    let old = f.commit(tree, vec![]);
    let new = f.commit(tree, vec![old, old, old]);
    let limits = PushLimits {
        max_ancestry_steps: 4,
        ..PushLimits::default()
    };
    assert!(matches!(
        f.prepare(vec![command("refs/heads/main", Some(old), new)], limits),
        Err(PushFailure::Limit("ancestry steps"))
    ));
}

#[test]
fn incomplete_multi_ref_report_keeps_unknown_distinct_from_rejected() {
    let f = Fixture::new();
    let id = f.blob();
    let p = f
        .prepare(
            vec![tag_command(id), command("refs/tags/other", None, id)],
            PushLimits::default(),
        )
        .unwrap();
    let bytes = response(&[b"unpack ok", b"ng refs/tags/test rejected"]);
    let Err(PushError::Uncertain { report, .. }) = run(&bytes, &p) else {
        panic!("expected incomplete report");
    };
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(b"rejected".to_vec()))
    );
    assert_eq!(report.refs[1].status, None);
    assert!(!report.all_succeeded());
}
