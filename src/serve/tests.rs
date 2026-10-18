use std::sync::atomic::AtomicBool;

use super::*;

#[test]
fn protocol_reads_version_parameter() {
    assert_eq!(Protocol::from_parameters(b"version=1"), Protocol::V0);
    assert_eq!(Protocol::from_parameters(b"version=2"), Protocol::V2);
    assert_eq!(Protocol::from_parameters(b"version=22"), Protocol::V0);
}

#[test]
fn v0_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let repo = crate::Repository::init(
        crate::ObjectFormat::Sha1,
        directory.path().join("repo.git"),
        crate::InitKind::Bare,
    )
    .unwrap();
    let upload = UploadPack::new(&repo).unwrap();
    let error = upload
        .serve(
            Protocol::V0,
            &mut &b""[..],
            &mut Vec::new(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error, ServeError::Unsupported("protocol v0")));
}
