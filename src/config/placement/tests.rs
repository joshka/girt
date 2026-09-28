use std::path::Path;

use rstest::rstest;

use super::*;
use crate::config::{Config, ConfigFile, ConfigInputs, ConfigScope, ResolveFailure};

fn inputs(root: &Path, source: &str) -> ConfigInputs {
    std::fs::write(root.join("config"), source).unwrap();
    for name in ["a", "b", "c", "d"] {
        std::fs::write(
            root.join(name),
            format!("[fixture \"{name}\"]\nvalue={name}\n"),
        )
        .unwrap();
    }
    ConfigInputs {
        files: vec![ConfigFile {
            path: root.join("config"),
            scope: ConfigScope::Local,
            optional: false,
        }],
        ..ConfigInputs::default()
    }
}

fn resolve(inputs: &ConfigInputs) -> Config {
    Config::resolve_with_include_placement(inputs, IncludePlacement::AfterSectionReverse).unwrap()
}

fn fixtures(config: &Config) -> Vec<&[u8]> {
    config
        .section_occurrences()
        .filter(|section| section.name() == b"fixture")
        .map(|section| section.subsection().unwrap())
        .collect()
}

#[rstest]
#[case::same_section("[include]\npath=a\npath=b\n", &[b"b".as_slice(), b"a"] as &[&[u8]])]
#[case::separate_sections("[include]\npath=a\n[include]\npath=b\n", &[b"a".as_slice(), b"b"] as &[&[u8]])]
#[case::repeated_child("[include]\npath=a\npath=b\npath=a\n", &[b"a".as_slice(), b"b", b"a"] as &[&[u8]])]
#[case::interleaved("[include]\npath=a\n[fixture \"middle\"]\nv=1\n[include]\npath=b\n", &[b"a".as_slice(), b"middle", b"b"] as &[&[u8]])]
#[case::matching_branch("[includeIf \"onbranch:main\"]\npath=a\npath=b\n", &[b"b".as_slice(), b"a"] as &[&[u8]])]
#[case::unmatched_branch("[includeIf \"onbranch:other\"]\npath=a\npath=b\n", &[])]
fn child_block_order(#[case] source: &str, #[case] expected: &[&[u8]]) {
    let root = tempfile::tempdir().unwrap();
    let mut inputs = inputs(root.path(), source);
    inputs.context.branch = Some(b"main".to_vec());
    assert_eq!(fixtures(&resolve(&inputs)), expected);
}

#[test]
fn nested_blocks_keep_membership_and_original_provenance() {
    let root = tempfile::tempdir().unwrap();
    let inputs = inputs(
        root.path(),
        "[include]\nmarker=before\npath=a\nmarker=between\npath=b\nmarker=after\n[empty]\n",
    );
    std::fs::write(
        root.path().join("a"),
        "[fixture \"a\"]\nvalue=a\n[include]\npath=c\npath=d\n",
    )
    .unwrap();
    let config = resolve(&inputs);
    assert_eq!(fixtures(&config), [b"b", b"a", b"d", b"c"]);
    assert_eq!(
        config.entries()[..5]
            .iter()
            .map(|entry| entry.name.as_slice())
            .collect::<Vec<_>>(),
        [b"marker".as_slice(), b"path", b"marker", b"path", b"marker"]
    );
    let mut members = Vec::new();
    for section in config.section_occurrences() {
        for &index in section.entry_indices() {
            assert_eq!(config.entries()[index].section, section.name());
            assert_eq!(
                config.entries()[index].subsection.as_deref(),
                section.subsection()
            );
            members.push(index);
        }
    }
    assert_eq!(members, (0..config.entries().len()).collect::<Vec<_>>());
    let c = config
        .entries()
        .iter()
        .find(|entry| entry.name == b"value" && entry.value.as_deref() == Some(b"c"))
        .unwrap();
    let origin = c.origin.as_ref().unwrap();
    assert_eq!(origin.scope, ConfigScope::Local);
    assert_eq!(
        origin.location.path.as_deref(),
        Some(root.path().join("c").as_path())
    );
    assert_eq!(origin.location.line, 2);
    assert_eq!(
        origin
            .included_from
            .iter()
            .map(|source| source.line)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(config.contains_section("empty", None));
    assert_eq!(
        config.section_occurrences().last().unwrap().name(),
        b"empty"
    );
    // Default placement still resumes parent members around expanded children.
    let default = Config::resolve(&inputs).unwrap();
    assert_eq!(fixtures(&default), [b"a", b"c", b"d", b"b"]);
    assert_ne!(default.entries()[2].name, b"marker");
}

#[test]
fn empty_headers_and_repeated_expansions_are_distinct() {
    let root = tempfile::tempdir().unwrap();
    let inputs = inputs(root.path(), "[include]\npath=a\npath=b\npath=a\n[tail]\n");
    std::fs::write(root.path().join("a"), "[fixture \"a\"]\n[fixture \"a2\"]\n").unwrap();
    let config = resolve(&inputs);
    assert_eq!(
        fixtures(&config),
        [b"a".as_slice(), b"a2", b"b", b"a", b"a2"]
    );
    assert_eq!(config.section_occurrences().count(), 7);
    assert_eq!(config.entries().len(), 4);
}

#[rstest]
#[case::direct_errors(false)]
#[case::nested_error_precedes_sibling(true)]
fn validation_stays_forward(#[case] nested: bool) {
    let root = tempfile::tempdir().unwrap();
    let inputs = inputs(root.path(), "[include]\npath=a\npath=b\n");
    std::fs::write(
        root.path().join("a"),
        if nested {
            "[include]\npath=c\n"
        } else {
            "[invalid"
        },
    )
    .unwrap();
    std::fs::write(root.path().join("b"), "[invalid").unwrap();
    std::fs::write(root.path().join("c"), "[invalid").unwrap();
    let error =
        Config::resolve_with_include_placement(&inputs, IncludePlacement::AfterSectionReverse)
            .unwrap_err();
    let ordinary = Config::resolve(&inputs).unwrap_err();
    assert!(matches!(error.source, ResolveFailure::Parse(_)));
    assert_eq!(error.location, ordinary.location);
    assert_eq!(error.included_from, ordinary.included_from);
    assert_eq!(
        error.location.path,
        Some(root.path().join(if nested { "c" } else { "a" }))
    );
}

#[rstest]
#[case::matching_hasconfig(true)]
#[case::unmatched_hasconfig(false)]
fn hasconfig_keeps_later_layer_scan(#[case] matching: bool) {
    let root = tempfile::tempdir().unwrap();
    let mut inputs = inputs(
        root.path(),
        "[includeIf \"hasconfig:remote.*.url:https://fixture/*\"]\npath=a\npath=b\n",
    );
    if matching {
        inputs.command =
            Some(Config::parse(b"[remote \"later\"]\nurl=https://fixture/repo\n").unwrap());
    }
    let config = resolve(&inputs);
    assert_eq!(
        fixtures(&config),
        if matching {
            vec![b"b".as_slice(), b"a"]
        } else {
            vec![]
        }
    );
    std::fs::write(
        root.path().join("a"),
        "[remote \"forbidden\"]\nurl=anything\n",
    )
    .unwrap();
    let error =
        Config::resolve_with_include_placement(&inputs, IncludePlacement::AfterSectionReverse)
            .unwrap_err();
    assert!(matches!(error.source, ResolveFailure::ConditionalRemote));
}

#[test]
fn placement_preserves_resource_failure_order() {
    let root = tempfile::tempdir().unwrap();
    let mut inputs = inputs(root.path(), "[include]\npath=a\npath=b\npath=a\n");
    std::fs::write(root.path().join("a"), "[include]\npath=c\n[empty]\n").unwrap();
    for bytes in [1, 20, 80, 128, 256, 400, 1024, 4096] {
        for entries in [0, 1, 4, 8, 16] {
            for depth in [0, 1, 2] {
                inputs.limits.bytes = bytes;
                inputs.limits.entries = entries;
                inputs.limits.depth = depth;
                let ordinary = Config::resolve(&inputs);
                let placed = Config::resolve_with_include_placement(
                    &inputs,
                    IncludePlacement::AfterSectionReverse,
                );
                match (ordinary, placed) {
                    (Ok(_), Ok(_)) => {}
                    (Err(left), Err(right)) => {
                        assert_eq!(left.to_string(), right.to_string());
                        assert_eq!(left.location, right.location);
                        assert_eq!(left.included_from, right.included_from);
                    }
                    _ => panic!("placement changed budget admission"),
                }
            }
        }
    }
}

#[test]
fn resolved_runtime_members_and_empty_headers_remain_owned() {
    let root = tempfile::tempdir().unwrap();
    let source = inputs(root.path(), "[include]\npath=a\nmarker=after\n[empty]\n");
    let mut captured = Config::resolve(&source).unwrap();
    // Replay includes as missing optional sources; test retained physical membership separately.
    std::fs::remove_file(root.path().join("a")).unwrap();
    // Runtime relative includes have no origin, so use the already-resolved snapshot's section
    // structure with an absolute directive for this explicit replay.
    for entry in &mut captured.entries {
        if entry.name == b"path" {
            entry.value = Some(root.path().join("a").to_str().unwrap().as_bytes().to_vec());
        }
    }
    let inputs = ConfigInputs {
        command: Some(captured),
        ..ConfigInputs::default()
    };
    let config = resolve(&inputs);
    assert_eq!(
        config
            .entries()
            .iter()
            .map(|entry| entry.name.as_slice())
            .collect::<Vec<_>>(),
        [b"path".as_slice(), b"marker", b"value"]
    );
    assert_eq!(config.section_occurrences().count(), 3);
    assert!(
        config
            .entries()
            .iter()
            .all(|entry| entry.origin.as_ref().unwrap().scope == ConfigScope::Command)
    );
}
