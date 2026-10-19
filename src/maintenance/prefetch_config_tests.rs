use std::sync::atomic::AtomicBool;

use super::prefetch_config::{InventoryError, InventoryLimits, inventory};
use crate::Config;

fn read(bytes: &[u8]) -> Vec<(Vec<u8>, bool)> {
    let config = Config::parse(bytes).unwrap();
    inventory(&config, InventoryLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .into_iter()
        .map(|remote| (remote.name.to_vec(), remote.skip))
        .collect()
}

#[test]
fn names_are_exact_in_first_entry_order() {
    assert_eq!(
        read(b"[remote \"empty\"]\n[remote \"z\"]\nurl=a\n[remote \"A\"]\nfetch=HEAD\n[remote \"z\"]\npush=HEAD\n[remote \"a\"]\nurl=b\n"),
        vec![(b"z".to_vec(), false), (b"A".to_vec(), false), (b"a".to_vec(), false)]
    );
}

#[test]
fn both_skip_spellings_follow_configuration_order() {
    assert_eq!(read(b"[remote \"o\"]\nskipFetchAll=true\nskipDefaultUpdate=false\n"), vec![(b"o".to_vec(), false)]);
    assert_eq!(read(b"[remote \"o\"]\nskipDefaultUpdate=false\nskipFetchAll=true\n"), vec![(b"o".to_vec(), true)]);
    assert_eq!(read(b"[REMOTE \"o\"]\nSKIPFETCHALL\n"), vec![(b"o".to_vec(), true)]);
}

#[test]
fn invalid_earlier_skip_value_is_not_hidden_by_override() {
    let config = Config::parse(b"[remote \"o\"]\nskipFetchAll=invalid\nskipDefaultUpdate=false\n").unwrap();
    assert!(matches!(inventory(&config, InventoryLimits::default(), &AtomicBool::new(false)), Err(InventoryError::Boolean { .. })));
}

#[test]
fn admission_precedes_interpretation_and_allocations() {
    let config = Config::parse(b"[remote \"o\"]\nskipFetchAll=invalid\n").unwrap();
    let mut limits = InventoryLimits::default();
    limits.max_entries = 0;
    assert!(matches!(inventory(&config, limits, &AtomicBool::new(false)), Err(InventoryError::Limit("config entries"))));
    limits = InventoryLimits::default();
    limits.max_bytes = 0;
    assert!(matches!(inventory(&config, limits, &AtomicBool::new(false)), Err(InventoryError::Limit("config bytes"))));
    limits = InventoryLimits::default();
    limits.max_remotes = 0;
    assert!(matches!(inventory(&config, limits, &AtomicBool::new(false)), Err(InventoryError::Limit("remotes"))));
    assert!(matches!(inventory(&config, InventoryLimits::default(), &AtomicBool::new(true)), Err(InventoryError::Cancelled)));
}

#[test]
fn unrelated_entries_are_charged_and_names_are_borrowed() {
    let config = Config::parse(b"[core]\nbare=true\n[remote \"o\"]\nurl=a\n").unwrap();
    let mut limits = InventoryLimits::default();
    limits.max_entries = 1;
    assert!(matches!(inventory(&config, limits, &AtomicBool::new(false)), Err(InventoryError::Limit("config entries"))));
    let result = inventory(&config, InventoryLimits::default(), &AtomicBool::new(false)).unwrap();
    let name = config.entries()[1].subsection.as_deref().unwrap();
    assert_eq!(result[0].name.as_ptr(), name.as_ptr());
}
