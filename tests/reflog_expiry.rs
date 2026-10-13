//! Expiry configuration observations generated against the installed Git executable.
use std::fs;
use std::process::Command;

use girt::Config;
use girt::refs::RefName;
use girt::retention::{ReflogCutoffs, ReflogExpiry};

fn git_expiry(value: &str) -> i128 {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    fs::write(&path, format!("[gc]\nreflogExpire = {value}\n")).unwrap();
    let output = Command::new("git")
        .args(["config", "--file"])
        .arg(path)
        .args(["--type=expiry-date", "--get", "gc.reflogExpire"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[rstest::rstest]
#[case::never("never")]
#[case::false_value("false")]
#[case::now("now")]
#[case::all("all")]
#[case::timestamp("1700000000")]
fn configured_cutoff_matches_git(#[case] value: &str) {
    let config = Config::parse(format!("[gc]\nreflogExpire = {value}\n").as_bytes()).unwrap();
    let expiry = ReflogExpiry::from_config(&config, 1_800_000_000).unwrap();
    let cutoffs = expiry
        .cutoffs(&RefName::new(b"refs/heads/main").unwrap())
        .unwrap();
    assert_eq!(cutoffs.general.unwrap_or(0), git_expiry(value));
}

#[rstest::rstest]
#[case::defaults(b"", b"refs/heads/main", Some(1_800_000_000 - 90 * 86400), Some(1_800_000_000 - 30 * 86400))]
#[case::stash(
    b"[gc]\nreflogExpire=now\nreflogExpireUnreachable=now\n",
    b"refs/stash",
    None,
    None
)]
#[case::first_pattern(
    b"[gc \"refs/*\"]\nreflogExpire=never\n[gc \"refs/heads/*\"]\nreflogExpire=now\n",
    b"refs/heads/nested/topic",
    None,
    None
)]
#[case::last_duplicate(b"[gc \"refs/*\"]\nreflogExpire=never\nreflogExpire=now\n", b"refs/stash", Some(u64::MAX as i128), None)]
#[case::relative(b"[gc]\nreflogExpire=2.weeks.ago\nreflogExpireUnreachable=3 days\n", b"HEAD", Some(1_800_000_000 - 14 * 86400), Some(1_800_000_000 - 3 * 86400))]
fn policy_precedence(
    #[case] bytes: &[u8],
    #[case] name: &[u8],
    #[case] reachable: Option<i128>,
    #[case] unreachable: Option<i128>,
) {
    let expiry = ReflogExpiry::from_config(&Config::parse(bytes).unwrap(), 1_800_000_000).unwrap();
    assert_eq!(
        expiry.cutoffs(&RefName::new(name).unwrap()).unwrap(),
        ReflogCutoffs {
            general: reachable,
            unreachable
        }
    );
}

#[rstest::rstest]
#[case::invalid(b"garbage")]
#[case::implicit(b"")]
#[case::overflow(b"9999999999999999999999999999999999999999 days")]
#[case::malformed_plural(b"2.dayss")]
#[case::upper_false(b"FALSE")]
fn invalid_expiry_refuses_policy(#[case] value: &[u8]) {
    let bytes = [b"[gc]\nreflogExpire=".as_slice(), value, b"\n"].concat();
    assert!(ReflogExpiry::from_config(&Config::parse(&bytes).unwrap(), 1_800_000_000).is_err());
}

#[rstest::rstest]
#[case::mixed_now("Now", 0)]
#[case::quoted_now("\" now \"", 0)]
#[case::seconds("5.seconds.ago", 5)]
#[case::minutes("2 minutes ago", 120)]
#[case::hours("3 hours", 10800)]
#[case::days("90.days.ago", 90 * 86400)]
#[case::weeks("2 weeks", 14 * 86400)]
fn clock_relative_values_match_git(#[case] value: &str, #[case] age: i128) {
    let before = git_expiry("0");
    let observed = git_expiry(value);
    let after = git_expiry("0");
    let config = Config::parse(format!("[gc]\nreflogExpire={value}\n").as_bytes()).unwrap();
    let expiry = ReflogExpiry::from_config(&config, before).unwrap();
    assert_eq!(
        expiry
            .cutoffs(&RefName::new(b"HEAD").unwrap())
            .unwrap()
            .general,
        Some(before - age)
    );
    assert!((before - age..=after - age).contains(&observed));
}
