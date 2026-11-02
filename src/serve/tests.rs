use super::*;

#[test]
fn protocol_reads_version_parameter() {
    assert_eq!(Protocol::from_parameters(b"version=1"), Protocol::V0);
    assert_eq!(Protocol::from_parameters(b"version=2"), Protocol::V2);
    assert_eq!(Protocol::from_parameters(b"version=22"), Protocol::V0);
}

#[test]
fn selection_limits_keep_their_name() {
    assert!(matches!(
        ServeError::from(crate::push::PushFailure::Limit("retained bytes")),
        ServeError::Limit("retained bytes")
    ));
    let read = crate::push::PushFailure::Read {
        id: crate::ObjectFormat::Sha1.empty_tree(),
        source: crate::ObjectReadError::Limit("object bytes"),
    };
    assert!(matches!(
        ServeError::from(read),
        ServeError::Limit("object bytes")
    ));
}
