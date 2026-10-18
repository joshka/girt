use super::*;

#[test]
fn protocol_reads_version_parameter() {
    assert_eq!(Protocol::from_parameters(b"version=1"), Protocol::V0);
    assert_eq!(Protocol::from_parameters(b"version=2"), Protocol::V2);
    assert_eq!(Protocol::from_parameters(b"version=22"), Protocol::V0);
}
