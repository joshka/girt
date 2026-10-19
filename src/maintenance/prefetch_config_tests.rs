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
    assert_eq!(
        read(b"[remote \"o\"]\nskipFetchAll=true\nskipDefaultUpdate=false\n"),
        vec![(b"o".to_vec(), false)]
    );
    assert_eq!(
        read(b"[remote \"o\"]\nskipDefaultUpdate=false\nskipFetchAll=true\n"),
        vec![(b"o".to_vec(), true)]
    );
    assert_eq!(
        read(b"[REMOTE \"o\"]\nSKIPFETCHALL\n"),
        vec![(b"o".to_vec(), true)]
    );
}

#[test]
fn invalid_earlier_skip_value_is_not_hidden_by_override() {
    let config =
        Config::parse(b"[remote \"o\"]\nskipFetchAll=invalid\nskipDefaultUpdate=false\n").unwrap();
    assert!(matches!(
        inventory(&config, InventoryLimits::default(), &AtomicBool::new(false)),
        Err(InventoryError::Boolean { .. })
    ));
}

#[test]
fn admission_precedes_interpretation_and_allocations() {
    let config = Config::parse(b"[remote \"o\"]\nskipFetchAll=invalid\n").unwrap();
    let limits = InventoryLimits {
        max_entries: 0,
        ..InventoryLimits::default()
    };
    assert!(matches!(
        inventory(&config, limits, &AtomicBool::new(false)),
        Err(InventoryError::Limit("config entries"))
    ));
    let limits = InventoryLimits {
        max_bytes: 0,
        ..InventoryLimits::default()
    };
    assert!(matches!(
        inventory(&config, limits, &AtomicBool::new(false)),
        Err(InventoryError::Limit("config bytes"))
    ));
    let limits = InventoryLimits {
        max_remotes: 0,
        ..InventoryLimits::default()
    };
    assert!(matches!(
        inventory(&config, limits, &AtomicBool::new(false)),
        Err(InventoryError::Limit("remotes"))
    ));
    assert!(matches!(
        inventory(&config, InventoryLimits::default(), &AtomicBool::new(true)),
        Err(InventoryError::Cancelled)
    ));
}

#[test]
fn unrelated_entries_are_charged_and_names_are_borrowed() {
    let config = Config::parse(b"[core]\nbare=true\n[remote \"o\"]\nurl=a\n").unwrap();
    let limits = InventoryLimits {
        max_entries: 1,
        ..InventoryLimits::default()
    };
    assert!(matches!(
        inventory(&config, limits, &AtomicBool::new(false)),
        Err(InventoryError::Limit("config entries"))
    ));
    let result = inventory(&config, InventoryLimits::default(), &AtomicBool::new(false)).unwrap();
    let name = config.entries()[1].subsection.as_deref().unwrap();
    assert_eq!(result[0].name.as_ptr(), name.as_ptr());
}

#[test]
fn inclusive_limits_and_empty_inventory() {
    let config = Config::parse(b"[remote \"o\"]\nurl=a\n").unwrap();
    let limits = InventoryLimits {
        max_entries: 1,
        max_bytes: 128 + 6 + 1 + 3 + 1,
        max_remotes: 1,
    };
    assert_eq!(
        inventory(&config, limits, &AtomicBool::new(false))
            .unwrap()
            .len(),
        1
    );
    let mut short = limits;
    short.max_bytes -= 1;
    assert!(matches!(
        inventory(&config, short, &AtomicBool::new(false)),
        Err(InventoryError::Limit("config bytes"))
    ));
    let zero = InventoryLimits {
        max_entries: 0,
        max_bytes: 0,
        max_remotes: 0,
    };
    assert!(
        inventory(&Config::parse(b"").unwrap(), zero, &AtomicBool::new(false))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn skip_flags_use_git_boolean_syntax() {
    assert_eq!(
        read(b"[remote \"o\"]\nskipFetchAll=0x1\n"),
        vec![(b"o".to_vec(), true)]
    );
    assert_eq!(
        read(b"[remote \"o\"]\nskipDefaultUpdate=\n"),
        vec![(b"o".to_vec(), false)]
    );
    assert_eq!(
        read(b"[core]\nskipFetchAll=invalid\n[remote \"o\"]\nurl=a\n"),
        vec![(b"o".to_vec(), false)]
    );
}
