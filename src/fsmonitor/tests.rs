use super::*;

#[test]
fn protocol_one_keeps_paths_and_captures_the_query_time() {
    let changes = response(1, 123, true, b"file\0dir/\0", Limits::default()).unwrap();
    assert_eq!(changes.token(), &MonitorToken::Timestamp(123));
    assert_eq!(changes.paths(), [b"file".to_vec(), b"dir/".to_vec()]);
    assert!(!changes.full_scan());
}

#[test]
fn protocol_two_requires_a_checkpoint_and_obeys_reset() {
    let changes = response(2, 0, true, b"token\0/\0", Limits::default()).unwrap();
    assert!(changes.full_scan());
    assert_eq!(changes.token(), &MonitorToken::Opaque(b"token".to_vec()));
    assert!(response(2, 0, true, b"token", Limits::default()).is_err());
    assert!(response(2, 0, true, b"\0file\0", Limits::default()).is_err());
    assert!(response(2, 0, true, b"token\0file", Limits::default()).is_err());
    assert!(
        response(2, 0, false, b"token\0", Limits::default())
            .unwrap()
            .full_scan()
    );
}

#[test]
fn changed_paths_and_counts_are_bounded_before_use() {
    for path in [
        b"../escape".as_slice(),
        b"/absolute",
        b"a/../b",
        b".git/index",
        b"a//b",
    ] {
        let mut bytes = b"token\0".to_vec();
        bytes.extend_from_slice(path);
        bytes.push(0);
        assert!(response(2, 0, true, &bytes, Limits::default()).is_err());
    }
    let limits = Limits {
        max_paths: 0,
        ..Limits::default()
    };
    assert!(matches!(
        response(2, 0, true, b"token\0file\0", limits),
        Err(Error::Limit("paths"))
    ));
}
