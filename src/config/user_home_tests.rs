use std::fs;
use std::path::PathBuf;

use rstest::rstest;

use super::{
    Config, ConfigFile, ConfigInputs, ConfigScope, IncludeConditionVisibility, IncludePlacement,
    ResolveFailure, ResolveOptions, UnresolvedIncludePath,
};

fn options(conditions: IncludeConditionVisibility) -> ResolveOptions {
    ResolveOptions {
        conditions,
        unresolved_paths: UnresolvedIncludePath::Skip,
        placement: IncludePlacement::AfterSectionReverse,
        ..Default::default()
    }
}

#[rstest]
#[case(IncludeConditionVisibility::AllInputs)]
#[case(IncludeConditionVisibility::RootSnapshot)]
fn lookup_is_lazy_ordered_memoized_and_explicit_context_wins(
    #[case] conditions: IncludeConditionVisibility,
) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("first"), "[include]\npath=~nested/leaf\n").unwrap();
    fs::write(root.path().join("leaf"), "[marker]\nvalue=found\n").unwrap();
    let body = b"[include]\npath=~first/first\npath=~missing/leaf\npath=~first/first\npath=~missing/leaf\npath=~explicit/leaf\n";
    fs::write(root.path().join("root"), body).unwrap();
    let input = ConfigInputs {
        files: vec![ConfigFile {
            path: root.path().join("root"),
            scope: ConfigScope::Global,
            optional: false,
        }],
        command: Some(Config::parse(body).unwrap()),
        context: super::IncludeContext {
            user_homes: vec![(b"explicit".to_vec(), root.path().into())],
            ..Default::default()
        },
        ..Default::default()
    };
    for _ in 0..2 {
        let mut calls = Vec::new();
        let config = Config::resolve_with_user_home_lookup(&input, options(conditions), |name| {
            calls.push(name.to_vec());
            (name != b"missing").then(|| root.path().into())
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![b"first".to_vec(), b"nested".to_vec(), b"missing".to_vec()]
        );
        assert_eq!(config.values("marker", None, "value").count(), 6);
        let leaf = config
            .entries()
            .iter()
            .find(|entry| entry.section == b"marker")
            .unwrap();
        assert_eq!(leaf.origin.as_ref().unwrap().included_from.len(), 1);
    }
}

#[rstest]
#[case(IncludeConditionVisibility::AllInputs, 1)]
#[case(IncludeConditionVisibility::RootSnapshot, 0)]
fn condition_policy_controls_when_lookup_is_reached(
    #[case] conditions: IncludeConditionVisibility,
    #[case] expected: usize,
) {
    let input = ConfigInputs {
        command: Some(
            Config::parse(
                b"[includeIf \"hasconfig:remote.*.url:https://absent/**\"]\npath=~unknown/child\n",
            )
            .unwrap(),
        ),
        ..Default::default()
    };
    let mut calls = 0;
    Config::resolve_with_user_home_lookup(&input, options(conditions), |_| {
        calls += 1;
        None
    })
    .unwrap();
    assert_eq!(calls, expected);
}

#[test]
fn no_named_expansion_means_no_callback() {
    let root = tempfile::tempdir().unwrap();
    let input = ConfigInputs {
        command: Some(
            Config::parse(b"[include]\npath=~/absent\npath=%(prefix)/absent\npath=~bare-name\n")
                .unwrap(),
        ),
        context: super::IncludeContext {
            home: Some(root.path().into()),
            prefix: Some(root.path().into()),
            ..Default::default()
        },
        ..Default::default()
    };
    Config::resolve_with_user_home_lookup(
        &input,
        options(IncludeConditionVisibility::RootSnapshot),
        |_| panic!("not a named-user expansion"),
    )
    .unwrap();
}

#[test]
fn oversized_callback_result_is_rejected_before_path_use() {
    let input = ConfigInputs {
        command: Some(Config::parse(b"[include]\npath=~u/child\n").unwrap()),
        limits: super::ResolveLimits {
            bytes: 1024,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut calls = 0;
    let error = Config::resolve_with_user_home_lookup(
        &input,
        options(IncludeConditionVisibility::RootSnapshot),
        |_| {
            calls += 1;
            Some(PathBuf::from("x".repeat(2048)))
        },
    )
    .unwrap_err();
    assert_eq!(calls, 1);
    assert!(matches!(
        error.source,
        ResolveFailure::Limit("user home cache bytes")
    ));
}

#[test]
fn malformed_path_never_reaches_callback() {
    let input = ConfigInputs {
        command: Some(
            Config::from_decoded_sections(
                vec![super::DecodedSection {
                    name: b"include".to_vec(),
                    subsection: None,
                    entries: vec![super::DecodedEntry {
                        name: b"path".to_vec(),
                        value: Some(b"~u/child\0".to_vec()),
                    }],
                }],
                super::ResolveLimits::default(),
            )
            .unwrap(),
        ),
        ..Default::default()
    };
    let error = Config::resolve_with_user_home_lookup(
        &input,
        options(IncludeConditionVisibility::RootSnapshot),
        |_| panic!("malformed path"),
    )
    .unwrap_err();
    assert!(matches!(error.source, ResolveFailure::Input(_)));
}

#[test]
fn explicit_relative_mapping_preserves_file_relative_semantics() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("root"), b"[include]\npath=~u/leaf\n").unwrap();
    fs::write(root.path().join("leaf"), b"[marker]\nvalue=yes\n").unwrap();
    let input = ConfigInputs {
        files: vec![ConfigFile {
            path: root.path().join("root"),
            scope: ConfigScope::Global,
            optional: false,
        }],
        context: super::IncludeContext {
            user_homes: vec![(b"u".to_vec(), PathBuf::new())],
            ..Default::default()
        },
        ..Default::default()
    };
    let config = Config::resolve_with_user_home_lookup(&input, ResolveOptions::default(), |_| {
        panic!("explicit mapping wins")
    })
    .unwrap();
    assert_eq!(
        config.value("marker", None, "value"),
        Some(Some(b"yes".as_slice()))
    );
}
