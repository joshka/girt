use std::fs;

use rstest::rstest;

use super::{
    Config, ConfigFile, ConfigInputs, ConfigScope, IncludeConditionVisibility,
    IncludeDirectiveCase, IncludePlacement, ResolveFailure, ResolveOptions, UnresolvedIncludePath,
};

fn options() -> ResolveOptions {
    ResolveOptions {
        placement: IncludePlacement::AfterSectionReverse,
        conditions: IncludeConditionVisibility::RootSnapshot,
        directive_case: IncludeDirectiveCase::Canonical,
        unresolved_paths: UnresolvedIncludePath::Skip,
    }
}

fn inputs(root: &std::path::Path, body: &str) -> ConfigInputs {
    fs::write(root.join("root"), body).unwrap();
    ConfigInputs {
        files: vec![ConfigFile {
            path: root.join("root"),
            scope: ConfigScope::Global,
            optional: false,
        }],
        ..ConfigInputs::default()
    }
}

fn has_marker(config: &Config) -> bool {
    config.value("marker", None, "value").is_some()
}

// Original standalone public-API observations: all own root URLs are visible, while other roots
// and incomplete included subtrees are not. No upstream implementation or fixture was consulted.
#[rstest]
#[case::later_root_url(
    "[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n[remote \"r\"]\nurl=https://h/repo\n",
    true
)]
#[case::earlier_root_url(
    "[remote \"r\"]\nurl=https://h/repo\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n",
    true
)]
#[case::completed_child(
    "[include]\npath=url\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n",
    true
)]
#[case::later_child(
    "[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n[include]\npath=url\n",
    false
)]
fn root_snapshot_visibility(#[case] body: &str, #[case] matched: bool) {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("url"),
        "[remote \"r\"]\nurl=https://h/repo\n",
    )
    .unwrap();
    fs::write(root.path().join("selected"), "[marker]\nvalue=yes\n").unwrap();
    let config = Config::resolve_with_options(&inputs(root.path(), body), options()).unwrap();
    assert_eq!(has_marker(&config), matched);
}

#[rstest]
#[case::child_own_url(
    "[remote \"r\"]\nurl=https://h/repo\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n"
)]
#[case::completed_grandchild(
    "[include]\npath=url\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n"
)]
fn subtree_visibility_remains_frozen_until_return(#[case] child: &str) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("child"), child).unwrap();
    fs::write(
        root.path().join("url"),
        "[remote \"r\"]\nurl=https://h/repo\n",
    )
    .unwrap();
    fs::write(root.path().join("selected"), "[marker]\nvalue=yes\n").unwrap();
    let input = inputs(root.path(), "[include]\npath=child\n");
    assert!(!has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
    let input = inputs(
        root.path(),
        "[include]\npath=child\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n",
    );
    assert!(has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
    let input = inputs(
        root.path(),
        "[remote \"r\"]\nurl=https://h/repo\n[include]\npath=child\n",
    );
    assert!(has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
}

#[test]
fn roots_are_isolated_and_default_still_scans_all_inputs() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("selected"), "[marker]\nvalue=yes\n").unwrap();
    fs::write(
        root.path().join("later"),
        "[remote \"r\"]\nurl=https://h/repo\n",
    )
    .unwrap();
    let mut input = inputs(
        root.path(),
        "[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n",
    );
    input.files.push(ConfigFile {
        path: root.path().join("later"),
        scope: ConfigScope::Global,
        optional: false,
    });
    assert!(!has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
    assert!(has_marker(&Config::resolve(&input).unwrap()));
    input.files.reverse();
    assert!(!has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
}

#[test]
fn root_snapshot_reads_only_selected_hasconfig_children_and_allows_their_urls() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("bad"), "[broken").unwrap();
    let input = inputs(
        root.path(),
        "[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=bad\n",
    );
    assert!(Config::resolve_with_options(&input, options()).is_ok());
    assert!(matches!(
        Config::resolve(&input).unwrap_err().source,
        ResolveFailure::Parse(_)
    ));
    fs::write(
        root.path().join("selected"),
        "[remote \"child\"]\nurl=https://h/child\n[marker]\nvalue=yes\n",
    )
    .unwrap();
    let input = inputs(
        root.path(),
        "[remote \"r\"]\nurl=https://h/repo\n[includeIf \"hasconfig:remote.*.url:https://h/**\"]\npath=selected\n",
    );
    assert!(has_marker(
        &Config::resolve_with_options(&input, options()).unwrap()
    ));
    assert!(matches!(
        Config::resolve(&input).unwrap_err().source,
        ResolveFailure::ConditionalRemote
    ));
}

#[rstest]
#[case("include", "PATH", true)]
#[case("Include", "path", false)]
#[case("includeIf \"hasconfig:remote.*.url:https://h/**\"", "Path", true)]
#[case("includeif \"hasconfig:remote.*.url:https://h/**\"", "path", false)]
#[case("includeIf \"hasconfig:REMOTE.*.url:https://h/**\"", "path", false)]
fn canonical_sections_preserve_case_insensitive_keys_and_url_lookup(
    #[case] section: &str,
    #[case] key: &str,
    #[case] matched: bool,
) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("selected"), "[marker]\nvalue=yes\n").unwrap();
    let body = format!("[REMOTE \"r\"]\nURL=https://h/repo\n[{section}]\n{key}=selected\n");
    assert_eq!(
        has_marker(&Config::resolve_with_options(&inputs(root.path(), &body), options()).unwrap()),
        matched
    );
}

#[rstest]
#[case("~/missing")]
#[case("~unknown/missing")]
#[case("%(prefix)/missing")]
fn absent_interpolation_context_is_skipped_without_dropping_entry(#[case] path: &str) {
    let root = tempfile::tempdir().unwrap();
    let input = inputs(root.path(), &format!("[include]\npath={path}\n"));
    let config = Config::resolve_with_options(&input, options()).unwrap();
    assert_eq!(
        config.value("include", None, "path"),
        Some(Some(path.as_bytes()))
    );
    assert!(matches!(
        Config::resolve(&input).unwrap_err().source,
        ResolveFailure::Input(_)
    ));
}

#[test]
fn runtime_sources_keep_reverse_placement_provenance_and_forward_error_order() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a"), "[marker]\nvalue=a\n").unwrap();
    fs::write(root.path().join("b"), "[marker]\nvalue=b\n").unwrap();
    let body = format!(
        "[include]\npath={}\npath={}\n",
        root.path().join("a").display(),
        root.path().join("b").display()
    );
    let input = ConfigInputs {
        command: Some(Config::parse(body.as_bytes()).unwrap()),
        ..ConfigInputs::default()
    };
    let config = Config::resolve_with_options(&input, options()).unwrap();
    assert_eq!(
        config.values("marker", None, "value").collect::<Vec<_>>(),
        vec![Some(b"b".as_slice()), Some(b"a".as_slice())]
    );
    let marker = config
        .entries()
        .iter()
        .find(|entry| entry.section == b"marker")
        .unwrap();
    assert_eq!(marker.origin.as_ref().unwrap().scope, ConfigScope::Command);
    assert_eq!(marker.origin.as_ref().unwrap().included_from.len(), 1);
    fs::write(root.path().join("a"), "[broken").unwrap();
    fs::write(root.path().join("b"), "[also-broken").unwrap();
    let error = Config::resolve_with_options(&input, options()).unwrap_err();
    assert_eq!(error.location.path, Some(root.path().join("a")));
}

#[rstest]
#[case::bytes(1024, 100, 512)]
#[case::entries(0, 0, 4096)]
fn runtime_root_url_view_is_bounded_before_cloning(
    #[case] value_bytes: usize,
    #[case] entries: usize,
    #[case] bytes: usize,
) {
    let body = format!("[remote \"r\"]\nurl={}\n", "x".repeat(value_bytes));
    let input = ConfigInputs {
        command: Some(Config::parse(body.as_bytes()).unwrap()),
        limits: super::ResolveLimits {
            bytes,
            entries,
            ..Default::default()
        },
        ..ConfigInputs::default()
    };
    assert!(matches!(
        Config::resolve_with_options(&input, options())
            .unwrap_err()
            .source,
        ResolveFailure::Limit(_)
    ));
}
