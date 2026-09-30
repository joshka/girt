use rstest::rstest;

use super::*;

fn find(body: &[u8]) -> Result<Option<ConfiguredRemote>, ConfiguredRemoteError> {
    ConfiguredRemote::find(&Config::parse(body).unwrap(), b"origin")
}

// Expected results come from original disposable public-gix 0.87.1 probes on macOS.
// These fixtures characterize configuration interpretation, not transport permission.
#[rstest]
#[case::absent(b"", None, None)]
#[case::empty_section(b"[remote \"origin\"]", None, None)]
#[case::tag_only(b"[remote \"origin\"]\ntagOpt=--tags", None, None)]
#[case::spec_only(
    b"[remote \"origin\"]\nfetch=+refs/heads/a:refs/remotes/origin/a",
    None,
    None
)]
#[case::reset_invalid(
    b"[remote \"origin\"]\nurl=host:\nurl=\nurl=good",
    Some(b"good".as_slice()),
    Some(b"good".as_slice())
)]
#[case::implicit_reset(
    b"[remote \"origin\"]\nurl=host:\nurl\nurl=good",
    Some(b"good".as_slice()),
    Some(b"good".as_slice())
)]
#[case::implicit_last(b"[remote \"origin\"]\nurl=good\nurl", None, None)]
#[case::push_reset(
    b"[remote \"origin\"]\npushurl=host:\npushurl\npushurl=good",
    None,
    Some(b"good".as_slice())
)]
#[case::first_values(
    b"[remote \"origin\"]\nurl=first\nurl=second\npushurl=pushfirst\npushurl=pushsecond",
    Some(b"first".as_slice()),
    Some(b"pushfirst".as_slice())
)]
#[case::tag_implicit(
    b"[remote \"origin\"]\ntagOpt\ntagOpt\nurl=good",
    Some(b"good".as_slice()),
    Some(b"good".as_slice())
)]
#[case::tag_replaced(
    b"[remote \"origin\"]\ntagOpt=bad\ntagOpt=--tags\nurl=good",
    Some(b"good".as_slice()),
    Some(b"good".as_slice())
)]
#[case::local_bytes(
    b"[remote \"origin\"]\nurl=/repo/\xff",
    Some(b"/repo/\xff".as_slice()),
    Some(b"/repo/\xff".as_slice())
)]
fn urls_and_activity(
    #[case] body: &[u8],
    #[case] fetch: Option<&[u8]>,
    #[case] push: Option<&[u8]>,
) {
    let result = find(body).unwrap();
    assert_eq!(result.as_ref().and_then(ConfiguredRemote::fetch_url), fetch);
    assert_eq!(result.as_ref().and_then(ConfiguredRemote::push_url), push);
    assert_eq!(result.is_some(), fetch.is_some() || push.is_some());
}

#[rstest]
#[case::implicit_match(
    b"[url \"new/\"]\ninsteadOf\n[remote \"origin\"]\nurl=repo",
    b"new/repo",
    b"new/repo"
)]
#[case::empty_match(
    b"[url \"new/\"]\ninsteadOf=\n[remote \"origin\"]\nurl=repo",
    b"new/repo",
    b"new/repo"
)]
#[case::normalized_host(
    b"[url \"new/\"]\ninsteadOf=https://host/\n[remote \"origin\"]\nurl=https://HOST/repo",
    b"new/repo",
    b"new/repo"
)]
#[case::raw_host_not_matched(
    b"[url \"new/\"]\ninsteadOf=https://HOST/\n[remote \"origin\"]\nurl=https://HOST/repo",
    b"https://host/repo",
    b"https://host/repo"
)]
#[case::first_tie(b"[url \"first/\"]\ninsteadOf=old\n[url \"second/\"]\ninsteadOf=old\n[remote \"origin\"]\nurl=oldrepo", b"first/repo", b"first/repo")]
#[case::longest(b"[url \"short/\"]\ninsteadOf=old\n[url \"long/\"]\ninsteadOf=oldrepo\n[remote \"origin\"]\nurl=oldrepo", b"long/", b"long/")]
#[case::push_invalid(
    b"[url \"host:\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good",
    b"good",
    b"good"
)]
#[case::push_invalid_original(b"[url \"host:\"]\npushInsteadOf=good\n[url \"ordinary\"]\ninsteadOf=good\n[remote \"origin\"]\nurl=good", b"ordinary", b"good")]
#[case::push_valid(
    b"[url \"changed\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good",
    b"good",
    b"changed"
)]
#[case::push_explicit(
    b"[url \"host:\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good\npushurl=other",
    b"good",
    b"other"
)]
#[case::invalid_authority_push_retains_original(
    b"[url \"http://bad host/\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good",
    b"good",
    b"good"
)]
#[case::missing_host_push_retains_original(
    b"[url \"http://user@/\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good",
    b"good",
    b"good"
)]
fn rewrites(#[case] body: &[u8], #[case] fetch: &[u8], #[case] push: &[u8]) {
    let remote = find(body).unwrap().unwrap();
    assert_eq!(remote.fetch_url(), Some(fetch));
    assert_eq!(remote.push_url(), Some(push));
}

#[rstest]
#[case::tag_first(
    b"[remote \"origin\"]\ntagOpt=bad\nurl=host:\nfetch=bad..ref",
    ConfiguredRemoteError::TagOption
)]
#[case::empty_tag(b"[remote \"origin\"]\ntagOpt=", ConfiguredRemoteError::TagOption)]
#[case::fetch_before_push(b"[remote \"origin\"]\nurl=host:\npushurl=host:", ConfiguredRemoteError::Url { key: "url", occurrence: 1, rewritten: false, source: ConfiguredUrlError::MissingPath })]
#[case::all_urls(b"[remote \"origin\"]\nurl=good\nurl=host:", ConfiguredRemoteError::Url { key: "url", occurrence: 2, rewritten: false, source: ConfiguredUrlError::MissingPath })]
#[case::after_reset_index(b"[remote \"origin\"]\nurl=old\nurl=\nurl=host:", ConfiguredRemoteError::Url { key: "url", occurrence: 3, rewritten: false, source: ConfiguredUrlError::MissingPath })]
#[case::push_before_specs(b"[remote \"origin\"]\npushurl=host:\nfetch=bad..ref", ConfiguredRemoteError::Url { key: "pushurl", occurrence: 1, rewritten: false, source: ConfiguredUrlError::MissingPath })]
#[case::url_free_spec(b"[remote \"origin\"]\nfetch=bad..ref", ConfiguredRemoteError::Refspec { key: "fetch", occurrence: 1, source: ConfiguredRefspecError::Name })]
#[case::fetch_before_push_spec(b"[remote \"origin\"]\nfetch=bad..ref\npush", ConfiguredRemoteError::Refspec { key: "fetch", occurrence: 1, source: ConfiguredRefspecError::Name })]
#[case::push_spec_before_rewrite(b"[url \"host:\"]\ninsteadOf=good\n[remote \"origin\"]\nurl=good\npush", ConfiguredRemoteError::Refspec { key: "push", occurrence: 1, source: ConfiguredRefspecError::EmptyPush })]
#[case::rewritten_second(b"[url \"host:\"]\ninsteadOf=second\n[remote \"origin\"]\nurl=first\nurl=second", ConfiguredRemoteError::Url { key: "url", occurrence: 2, rewritten: true, source: ConfiguredUrlError::MissingPath })]
#[case::explicit_push_rewrite(b"[url \"host:\"]\ninsteadOf=push\n[remote \"origin\"]\nurl=good\npushurl=push", ConfiguredRemoteError::Url { key: "pushurl", occurrence: 1, rewritten: true, source: ConfiguredUrlError::MissingPath })]
#[case::unsupported(b"[remote \"origin\"]\nurl=foo::repo", ConfiguredRemoteError::UnsupportedUrlSyntax { key: "url", occurrence: 1, rewritten: false })]
#[case::invalid_before_unsupported(b"[remote \"origin\"]\nurl=host:\nurl=foo::repo", ConfiguredRemoteError::Url { key: "url", occurrence: 1, rewritten: false, source: ConfiguredUrlError::MissingPath })]
#[case::unsupported_before_spec(b"[remote \"origin\"]\nurl=foo::repo\nfetch=bad..ref", ConfiguredRemoteError::UnsupportedUrlSyntax { key: "url", occurrence: 1, rewritten: false })]
#[case::unsupported_push_rewrite(b"[url \"foo::repo\"]\npushInsteadOf=good\n[remote \"origin\"]\nurl=good", ConfiguredRemoteError::UnsupportedUrlSyntax { key: "url", occurrence: 1, rewritten: true })]
fn validation_order(#[case] body: &[u8], #[case] expected: ConfiguredRemoteError) {
    assert_eq!(find(body).unwrap_err(), expected);
}

#[test]
fn configured_refspecs_keep_occurrences_and_implicit_head() {
    let remote = find(b"[remote \"origin\"]\nurl=good\nfetch=main:\nfetch\nfetch=+main\npush=^refs/heads/private\n").unwrap().unwrap();
    assert_eq!(
        remote
            .fetch_refspecs()
            .iter()
            .map(ConfiguredRefspec::to_bytes)
            .collect::<Vec<_>>(),
        [b"main".to_vec(), b"HEAD".to_vec(), b"main".to_vec()]
    );
    assert_eq!(remote.push_refspecs()[0].to_bytes(), b"^refs/heads/private");
}

#[test]
fn remote_names_are_not_reference_names() {
    let config = Config::parse(b"[remote \"bad name\"]\nurl=good\n").unwrap();
    assert!(
        ConfiguredRemote::find(&config, b"bad name")
            .unwrap()
            .is_some()
    );
}

#[test]
fn debug_and_errors_do_not_disclose_urls() {
    let remote = find(b"[remote \"origin\"]\nurl=ssh://secret@host/repo\n")
        .unwrap()
        .unwrap();
    assert!(!format!("{remote:?}").contains("secret"));
    let error = find(b"[remote \"origin\"]\nurl=ssh://secret@host:\n").unwrap_err();
    assert!(!format!("{error:?} {error}").contains("secret"));
}

#[rstest]
#[case::absent(b"", false)]
#[case::other(b"[remote \"other\"]", false)]
#[case::empty(b"[remote \"origin\"]", true)]
#[case::unknown_field(b"[remote \"origin\"]\ncustom=value", true)]
#[case::reset(b"[remote \"origin\"]\nurl=bad:\nurl", true)]
#[case::spec_only(b"[remote \"origin\"]\nfetch", true)]
fn configured_record_retains_inactive_existence(#[case] body: &[u8], #[case] exists: bool) {
    let config = Config::parse(body).unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin").unwrap();
    assert_eq!(record.is_some(), exists);
    assert!(
        ConfiguredRemote::find(&config, b"origin")
            .unwrap()
            .is_none()
    );
}

#[rstest]
#[case::absent(b"", None)]
#[case::tags(b"tagOpt=--tags", Some(b"--tags".as_slice()))]
#[case::last(b"tagOpt=--tags\ntagOpt=--no-tags", Some(b"--no-tags".as_slice()))]
#[case::implicit(b"tagOpt\ntagOpt", None)]
fn configured_record_tag_option(#[case] body: &[u8], #[case] expected: Option<&[u8]>) {
    let config = Config::parse(&[b"[remote \"origin\"]\n", body].concat()).unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(record.tag_option(), expected);
}

#[test]
fn configured_record_retains_order_duplicates_and_canonical_raw_urls() {
    let config = Config::parse(b"[remote \"origin\"]\nurl=host:\nurl=\nurl=https://HOST:00443/a\nurl=second\nurl=second\npushurl=bad:\npushurl\npushurl=user@HOST:repo\nfetch\nfetch\npush=:refs/heads/old\ntagOpt=--tags\n[url \"host:\"]\ninsteadOf=https://host:443/\npushInsteadOf=second\n").unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(
        record.fetch_urls().collect::<Vec<_>>(),
        [b"https://host:443/a".as_slice(), b"second", b"second"]
    );
    assert_eq!(
        record.push_urls().collect::<Vec<_>>(),
        [b"user@host:repo".as_slice()]
    );
    assert_eq!(record.fetch_refspecs().len(), 2);
    assert_eq!(record.fetch_refspecs()[0], record.fetch_refspecs()[1]);
    assert_eq!(record.push_refspecs()[0].to_bytes(), b":refs/heads/old");
    assert_eq!(record.tag_option(), Some(b"--tags".as_slice()));
    assert!(ConfiguredRemote::find(&config, b"origin").is_ok());
}

#[test]
fn configured_record_does_not_validate_rewrites_or_apply_push_fallback() {
    let config = Config::parse(b"[remote \"origin\"]\nurl=secret-local-path\n[url \"host:\"]\ninsteadOf=secret-local-path\n").unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(
        record.fetch_urls().collect::<Vec<_>>(),
        [b"secret-local-path".as_slice()]
    );
    assert_eq!(record.push_urls().len(), 0);
    assert!(!format!("{record:?}").contains("secret-local-path"));
    assert!(matches!(
        ConfiguredRemote::find(&config, b"origin"),
        Err(ConfiguredRemoteError::Url {
            rewritten: true,
            ..
        })
    ));
}

#[rstest]
#[case::tag(b"tagOpt=bad\nurl=host:", "tag")]
#[case::fetch_url(b"url=host:\npushurl=host:\nfetch=bad?", "url")]
#[case::push_url(b"pushurl=host:\nfetch=bad?", "pushurl")]
#[case::fetch_spec(b"fetch=bad?\npush=bad?", "fetch")]
#[case::push_spec(b"push=bad?", "push")]
fn configured_record_preserves_error_order(#[case] body: &[u8], #[case] key: &str) {
    let config = Config::parse(&[b"[remote \"origin\"]\n", body].concat()).unwrap();
    let error = ConfiguredRemoteRecord::find(&config, b"origin").unwrap_err();
    let actual = match error {
        ConfiguredRemoteError::TagOption => "tag",
        ConfiguredRemoteError::Url { key, .. } | ConfiguredRemoteError::Refspec { key, .. } => key,
        error => panic!("unexpected error: {error}"),
    };
    assert_eq!(actual, key);
}

// Original public-gix 0.87.1 whole-remote fixtures qualified section-local tag selection.
#[rstest]
#[case::same_section(b"tagOpt=--tags\ntagOpt", None)]
#[case::same_section_invalid(b"tagOpt=bad\ntagOpt", None)]
#[case::different_sections(b"tagOpt=--tags\n[remote \"origin\"]\ntagOpt", Some(b"--tags".as_slice()))]
#[case::masked_bad(b"tagOpt=bad\n[remote \"origin\"]\ntagOpt=--no-tags", Some(b"--no-tags".as_slice()))]
#[case::explicit_empty_masked(b"tagOpt=\ntagOpt", None)]
#[case::non_utf8_masked(b"tagOpt=\xff\ntagOpt", None)]
#[case::case_fold(b"TAGOPT=--tags\n[REMOTE \"origin\"]\ntagopt", Some(b"--tags".as_slice()))]
fn implicit_tag_skips_only_its_physical_section(
    #[case] body: &[u8],
    #[case] expected: Option<&[u8]>,
) {
    let config = Config::parse(&[b"[remote \"origin\"]\nurl=repo\n", body].concat()).unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(record.tag_option(), expected);
    assert!(
        ConfiguredRemote::find(&config, b"origin")
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::winning_bad(b"tagOpt=bad\n[remote \"origin\"]\ntagOpt", "tag")]
#[case::winning_empty(b"tagOpt=\n[remote \"origin\"]\ntagOpt", "tag")]
#[case::winning_non_utf8(b"tagOpt=\xff\n[remote \"origin\"]\ntagOpt", "tag")]
#[case::masked_bad_url(b"tagOpt=bad\ntagOpt\nurl=host:", "url")]
#[case::masked_bad_unsupported(b"tagOpt=bad\ntagOpt\nurl=foo::repo", "unsupported")]
#[case::masked_bad_spec(b"tagOpt=bad\ntagOpt\nfetch=bad?", "fetch")]
fn physical_tag_selection_preserves_validation_order(#[case] body: &[u8], #[case] expected: &str) {
    let config = Config::parse(&[b"[remote \"origin\"]\n", body].concat()).unwrap();
    let error = ConfiguredRemoteRecord::find(&config, b"origin").unwrap_err();
    assert_eq!(
        ConfiguredRemote::find(&config, b"origin").unwrap_err(),
        error
    );
    let actual = match error {
        ConfiguredRemoteError::TagOption => "tag",
        ConfiguredRemoteError::Url { key, .. } | ConfiguredRemoteError::Refspec { key, .. } => key,
        ConfiguredRemoteError::UnsupportedUrlSyntax { .. } => "unsupported",
        error => panic!("unexpected error: {error}"),
    };
    assert_eq!(actual, expected);
}

#[test]
fn included_invalid_tag_then_implicit_still_fails_first() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::write(&child, b"[remote \"origin\"]\ntagOpt=bad\n").unwrap();
    let parent = root.path().join("parent");
    std::fs::write(
        &parent,
        b"[include]\npath=child\n[remote \"origin\"]\ntagOpt\nurl=host:\n",
    )
    .unwrap();
    let config = Config::resolve(&crate::config::ConfigInputs {
        files: vec![crate::config::ConfigFile {
            path: parent,
            scope: crate::config::ConfigScope::Global,
            optional: false,
        }],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        ConfiguredRemoteRecord::find(&config, b"origin").unwrap_err(),
        ConfiguredRemoteError::TagOption,
    );
}

#[rstest]
#[case::child_implicit(
    b"[remote \"origin\"]\ntagOpt=--tags\n[include]\npath=child\n",
    b"[remote \"origin\"]\ntagOpt\n",
    b"--tags"
)]
#[case::repeated_child(
    b"[include]\npath=child\npath=child\n[remote \"origin\"]\ntagOpt\n",
    b"[remote \"origin\"]\ntagOpt=--tags\n[remote \"origin\"]\ntagOpt\n",
    b"--tags"
)]
#[case::resumed_include(
    b"[include]\npath=child\nunused=after\n[remote \"origin\"]\ntagOpt\n",
    b"[remote \"origin\"]\ntagOpt=--no-tags\n",
    b"--no-tags"
)]
#[case::local_masks_child(
    b"[include]\npath=child\n[remote \"origin\"]\ntagOpt=--tags\n",
    b"[remote \"origin\"]\ntagOpt=bad\n",
    b"--tags"
)]
fn included_tag_sections_keep_their_membership(
    #[case] parent: &[u8],
    #[case] child: &[u8],
    #[case] expected: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("child"), child).unwrap();
    let path = root.path().join("parent");
    std::fs::write(&path, parent).unwrap();
    let config = Config::resolve(&crate::config::ConfigInputs {
        files: vec![crate::config::ConfigFile {
            path,
            scope: crate::config::ConfigScope::Global,
            optional: false,
        }],
        ..Default::default()
    })
    .unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(record.tag_option(), Some(expected));
    assert!(
        ConfiguredRemote::find(&config, b"origin")
            .unwrap()
            .is_none()
    );
}

#[rstest]
#[case::implicit(b"tagOpt", Some(b"--tags".as_slice()))]
#[case::masked_bad(b"tagOpt=bad\ntagOpt", Some(b"--tags".as_slice()))]
#[case::explicit(b"tagOpt=--no-tags", Some(b"--no-tags".as_slice()))]
#[case::empty(b"tagOpt=", None)]
fn runtime_tag_sections_follow_the_same_selection(
    #[case] command: &[u8],
    #[case] expected: Option<&[u8]>,
) {
    let environment = Config::from_environment(
        |key| match key {
            "GIT_CONFIG_COUNT" => Some(b"1".to_vec()),
            "GIT_CONFIG_KEY_0" => Some(b"remote.origin.tagOpt".to_vec()),
            "GIT_CONFIG_VALUE_0" => Some(b"--tags".to_vec()),
            _ => None,
        },
        1,
    )
    .unwrap();
    let command = Config::parse(&[b"[remote \"origin\"]\n", command].concat()).unwrap();
    let config = Config::resolve(&crate::config::ConfigInputs {
        environment: Some(environment),
        command: Some(command),
        ..Default::default()
    })
    .unwrap();
    let result = ConfiguredRemoteRecord::find(&config, b"origin");
    if let Some(expected) = expected {
        assert_eq!(result.unwrap().unwrap().tag_option(), Some(expected));
    } else {
        assert_eq!(result.unwrap_err(), ConfiguredRemoteError::TagOption);
    }
}

// Prefix selection was characterized with original public-gix 0.87.1 add-remote probes.
// Additional cases exercise the native byte-path and single-rewrite contracts.
#[rstest]
#[case::no_match(b"", b"https://HOST:00443/repo", b"https://host:443/repo")]
#[case::ordinary(b"[url \"rewritten/\"]\ninsteadOf=raw", b"rawrepo", b"rewritten/repo")]
#[case::normalized_match(
    b"[url \"changed/\"]\ninsteadOf=https://host:443/",
    b"https://HOST:00443/repo",
    b"changed/repo"
)]
#[case::raw_does_not_match(
    b"[url \"changed/\"]\ninsteadOf=https://HOST:00443/",
    b"https://HOST:00443/repo",
    b"https://host:443/repo"
)]
#[case::longest(
    b"[url \"short/\"]\ninsteadOf=raw\n[url \"long/\"]\ninsteadOf=rawrepo",
    b"rawrepo",
    b"long/"
)]
#[case::first_tie(
    b"[url \"first/\"]\ninsteadOf=raw\n[url \"second/\"]\ninsteadOf=raw",
    b"rawrepo",
    b"first/repo"
)]
#[case::empty_prefix(b"[url \"prefix/\"]\ninsteadOf=", b"repo", b"prefix/repo")]
#[case::implicit_prefix(b"[url \"prefix/\"]\ninsteadOf", b"repo", b"prefix/repo")]
#[case::push_ignored(b"[url \"push/\"]\npushInsteadOf=raw", b"rawrepo", b"rawrepo")]
#[case::invalid_push_ignored(b"[url \"host:\"]\npushInsteadOf=raw", b"raw", b"raw")]
#[case::output_normalized(
    b"[url \"ssh://HOST:00022/\"]\ninsteadOf=raw",
    b"rawrepo",
    b"ssh://host:22/repo"
)]
#[case::not_recursive(
    b"[url \"second/\"]\ninsteadOf=first/\n[url \"third/\"]\ninsteadOf=second/",
    b"first/repo",
    b"second/repo"
)]
#[case::local_bytes(b"[url \"/new/\"]\ninsteadOf=/old/", b"/old/\xff", b"/new/\xff")]
fn ordinary_url_rewrite(#[case] body: &[u8], #[case] input: &[u8], #[case] expected: &[u8]) {
    let config = Config::parse(body).unwrap();
    assert_eq!(
        crate::remote::rewrite_configured_url(&config, input).unwrap(),
        expected
    );
}

#[rstest]
#[case::empty_original(b"[url \"valid\"]\ninsteadOf=", b"", ConfiguredUrlError::MissingPath)]
#[case::invalid_original(
    b"[url \"valid\"]\ninsteadOf=host:",
    b"host:",
    ConfiguredUrlError::MissingPath
)]
#[case::invalid_replacement(
    b"[url \"host:\"]\ninsteadOf=raw",
    b"raw",
    ConfiguredUrlError::MissingPath
)]
#[case::empty_replacement(b"[url \"\"]\ninsteadOf=raw", b"raw", ConfiguredUrlError::MissingPath)]
#[case::unsupported_original(
    b"[url \"valid\"]\ninsteadOf=foo::repo",
    b"foo::repo",
    ConfiguredUrlError::Unsupported
)]
#[case::unsupported_replacement(
    b"[url \"foo::repo\"]\ninsteadOf=raw",
    b"raw",
    ConfiguredUrlError::Unsupported
)]
#[case::private_replacement(
    b"[url \"https://user:secret@host:bad/repo\"]\ninsteadOf=raw",
    b"raw",
    ConfiguredUrlError::Port
)]
fn ordinary_url_rewrite_errors(
    #[case] body: &[u8],
    #[case] input: &[u8],
    #[case] expected: ConfiguredUrlError,
) {
    let config = Config::parse(body).unwrap();
    let error = crate::remote::rewrite_configured_url(&config, input).unwrap_err();
    assert_eq!(error, expected);
    assert!(!format!("{error:?}: {error}").contains("secret"));
}

#[rstest]
#[case::literal_match(b"[url \"changed/\"]\ninsteadOf=https://host/a%2f", b"changed/repo")]
#[case::hex_case_sensitive(
    b"[url \"changed/\"]\ninsteadOf=https://host/a%2F",
    b"https://host/a%2frepo"
)]
#[case::decoded_no_match(
    b"[url \"changed/\"]\ninsteadOf=https://host/a/",
    b"https://host/a%2frepo"
)]
fn percent_rewrite_matches_serialized_bytes(#[case] body: &[u8], #[case] expected: &[u8]) {
    let config =
        Config::parse(&[body, b"\n[remote \"origin\"]\nurl=https://HOST/a%2frepo"].concat())
            .unwrap();
    assert_eq!(
        crate::remote::rewrite_configured_url(&config, b"https://HOST/a%2frepo").unwrap(),
        expected
    );
    assert_eq!(
        ConfiguredRemote::find(&config, b"origin")
            .unwrap()
            .unwrap()
            .fetch_url(),
        Some(expected)
    );
    assert_eq!(
        ConfiguredRemoteRecord::find(&config, b"origin")
            .unwrap()
            .unwrap()
            .fetch_urls()
            .next(),
        Some(b"https://host/a%2frepo".as_slice())
    );
}

#[rstest]
#[case::ordinary(b"insteadOf", b"https://host/a%20b/repo", b"https://host/a%20b/repo")]
#[case::push_only(b"pushInsteadOf", b"rawrepo", b"https://host/a%20b/repo")]
fn percent_rewrite_replacement(#[case] key: &[u8], #[case] fetch: &[u8], #[case] push: &[u8]) {
    let config = Config::parse(
        &[
            b"[url \"https://HOST/a%20b/\"]\n",
            key,
            b"=raw\n[remote \"origin\"]\nurl=rawrepo",
        ]
        .concat(),
    )
    .unwrap();
    let remote = ConfiguredRemote::find(&config, b"origin").unwrap().unwrap();
    assert_eq!(remote.fetch_url(), Some(fetch));
    assert_eq!(remote.push_url(), Some(push));
}

#[test]
fn percent_url_reset_and_validation_order() {
    let config = Config::parse(b"[remote \"origin\"]\nurl=https://host/a%GG\nurl=\nurl=https://HOST/a%20b\nfetch=bad..spec").unwrap();
    assert!(matches!(
        ConfiguredRemote::find(&config, b"origin"),
        Err(ConfiguredRemoteError::Refspec { key: "fetch", .. })
    ));
}

#[test]
fn tag_error_precedes_invalid_path_escape() {
    let config = Config::parse(b"[remote \"origin\"]\ntagOpt=bad\nurl=https://host/a%GG").unwrap();
    assert_eq!(
        ConfiguredRemote::find(&config, b"origin").unwrap_err(),
        ConfiguredRemoteError::TagOption
    );
}

#[rstest]
#[case::original(b"[remote \"origin\"]\nurl=\nurl=https://host/private%GG", 2, false)]
#[case::rewritten(
    b"[url \"https://host/private%GG\"]\ninsteadOf=raw\n[remote \"origin\"]\nurl=raw",
    1,
    true
)]
fn path_escape_error_retains_stage_without_values(
    #[case] body: &[u8],
    #[case] occurrence: usize,
    #[case] rewritten: bool,
) {
    let error = find(body).unwrap_err();
    assert_eq!(
        error,
        ConfiguredRemoteError::Url {
            key: "url",
            occurrence,
            rewritten,
            source: ConfiguredUrlError::PathEscape
        }
    );
    assert!(!format!("{error:?}: {error}").contains("private"));
}

#[test]
fn invalid_push_path_rewrite_retains_original() {
    let config = Config::parse(
        b"[url \"https://host/a%GG\"]\npushInsteadOf=raw\n[remote \"origin\"]\nurl=raw",
    )
    .unwrap();
    let remote = ConfiguredRemote::find(&config, b"origin").unwrap().unwrap();
    assert_eq!(remote.push_url(), Some(b"raw".as_slice()));
}

#[test]
fn refspec_error_precedes_invalid_rewritten_path() {
    let config = Config::parse(b"[url \"https://host/a%GG\"]\ninsteadOf=raw\n[remote \"origin\"]\nurl=raw\nfetch=bad..spec").unwrap();
    assert!(matches!(
        ConfiguredRemote::find(&config, b"origin"),
        Err(ConfiguredRemoteError::Refspec { key: "fetch", .. })
    ));
}

#[test]
fn configured_file_spaces_preserve_storage_and_rewrite_matching() {
    let config = Config::parse(b"[remote \"origin\"]\nurl=\"file:///repo  name \"\n[url \"file:///new place/\"]\ninsteadOf=\"file:///repo  \"\n").unwrap();
    let record = ConfiguredRemoteRecord::find(&config, b"origin")
        .unwrap()
        .unwrap();
    assert_eq!(
        record.fetch_urls().next(),
        Some(b"file:///repo  name ".as_slice())
    );
    let remote = ConfiguredRemote::find(&config, b"origin").unwrap().unwrap();
    assert_eq!(
        remote.fetch_url(),
        Some(b"file:///new place/name ".as_slice())
    );
    assert_eq!(
        crate::remote::rewrite_configured_url(&config, b"file:///repo  name ").unwrap(),
        b"file:///new place/name "
    );
}

#[rstest]
#[case::credentials(
    b"[remote \"origin\"]\nurl=https://user:secret@HOST/repo\n",
    b"https://user:secret@host/repo",
    b"https://user:secret@host/repo"
)]
#[case::ipv6_push(
    b"[remote \"origin\"]\nurl=relative/../repo\npushurl=ssh://[::1]/repo\n",
    b"relative/../repo",
    b"ssh://[::1]/repo"
)]
#[case::reset_invalid_credentials(
    b"[remote \"origin\"]\nurl=https://user:secret@host:bad/repo\nurl=\nurl=HTTPS://HOST/repo\n",
    b"HTTPS://host/repo",
    b"HTTPS://host/repo"
)]
#[case::ordinary_rewrite_credentials(
    b"[url \"https://user:secret@HOST/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=alias:repo\n",
    b"https://user:secret@host/repo",
    b"https://user:secret@host/repo"
)]
#[case::push_rewrite_custom(
    b"[url \"custom://HOST/\"]\npushInsteadOf=local/\n[remote \"origin\"]\nurl=local/repo\n",
    b"local/repo",
    b"custom://host/repo"
)]
#[case::query_and_fragment(
    b"[remote \"origin\"]\nurl=\"https://HOST/repo?query#fragment\"\n",
    b"https://host/repo?query#fragment",
    b"https://host/repo?query#fragment"
)]
#[case::repeated_sections(
    b"[remote \"origin\"]\nurl=HTTPS://HOST/first\n[remote \"origin\"]\nurl=ssh://[::1]/second\n",
    b"HTTPS://host/first",
    b"HTTPS://host/first"
)]
fn broader_urls_preserve_remote_resolution(
    #[case] body: &[u8],
    #[case] fetch: &[u8],
    #[case] push: &[u8],
) {
    let remote = find(body).unwrap().unwrap();
    assert_eq!(remote.fetch_url(), Some(fetch));
    assert_eq!(remote.push_url(), Some(push));
}

#[rstest]
#[case::second_fetch(
    b"[remote \"origin\"]\nurl=https://user:secret@host/repo\nurl=https://[::1]:bad/repo\npushurl=host:\n",
    "url", 2, false, ConfiguredUrlError::Port
)]
#[case::push_before_refspec(
    b"[remote \"origin\"]\nurl=custom://host/repo\npushurl=https://user:secret@host:bad/repo\nfetch=bad..ref\n",
    "pushurl", 1, false, ConfiguredUrlError::Port
)]
#[case::fetch_rewrite(
    b"[url \"https://user:secret@HOST:bad/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=alias:repo\n",
    "url", 1, true, ConfiguredUrlError::Port
)]
#[case::escaped_path(
    b"[remote \"origin\"]\nurl=https://user:secret@HOST/%ZZ\n",
    "url",
    1,
    false,
    ConfiguredUrlError::PathEscape
)]
fn broader_urls_preserve_occurrence_errors(
    #[case] body: &[u8],
    #[case] key: &'static str,
    #[case] occurrence: usize,
    #[case] rewritten: bool,
    #[case] source: ConfiguredUrlError,
) {
    let error = find(body).unwrap_err();
    assert_eq!(
        error,
        ConfiguredRemoteError::Url {
            key,
            occurrence,
            rewritten,
            source
        }
    );
    assert!(!format!("{error:?}: {error}").contains("secret"));
}

// Selected parsed components must survive serialization that loses syntax such as SSH brackets.
#[rstest]
#[case::ssh_bracket(
    b"[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit",
    b"ssh://HOST/repo%2Egit",
    b"HOST",
    b"/repo.git"
)]
#[case::scp_bracket(
    b"[remote \"origin\"]\nurl=[HOST]:repo%2Egit",
    b"HOST:repo%2Egit",
    b"HOST",
    b"repo%2Egit"
)]
#[case::custom_bracket(
    b"[remote \"origin\"]\nurl=custom://[HOST]/repo%2Egit",
    b"custom://%5BHOST%5D/repo%2Egit",
    b"[HOST]",
    b"/repo.git"
)]
#[case::custom_escape(
    b"[remote \"origin\"]\nurl=custom://HOST%3Fx/repo%2Egit",
    b"custom://HOST%3Fx/repo%2Egit",
    b"HOST?x",
    b"/repo.git"
)]
#[case::ordinary_rewrite(
    b"[url \"ssh://[HOST]/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=alias:repo%2Egit",
    b"ssh://HOST/repo%2Egit",
    b"HOST",
    b"/repo.git"
)]
#[case::reset(
    b"[remote \"origin\"]\nurl=host:\nurl=\nurl=ssh://[HOST]/repo%2Egit",
    b"ssh://HOST/repo%2Egit",
    b"HOST",
    b"/repo.git"
)]
#[case::first_surviving(
    b"[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit\nurl=ssh://other/second",
    b"ssh://HOST/repo%2Egit",
    b"HOST",
    b"/repo.git"
)]
fn selected_parts_retain_parse_provenance(
    #[case] body: &[u8],
    #[case] bytes: &[u8],
    #[case] host: &[u8],
    #[case] path: &[u8],
) {
    let remote = find(body).unwrap().unwrap();
    assert_eq!(remote.fetch_url(), Some(bytes));
    let parts = remote.fetch_url_parts().unwrap();
    assert_eq!(parts.host(), Some(host));
    assert_eq!(parts.path(), path);
    assert_eq!(remote.push_url(), remote.fetch_url());
    assert_eq!(remote.push_url_parts(), remote.fetch_url_parts());
}

#[rstest]
#[case::push_rewrite(
    b"[url \"ssh://[PUSH]/\"]\npushInsteadOf=ssh://HOST/\n[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit",
    b"HOST", b"PUSH", b"ssh://PUSH/repo%2Egit"
)]
#[case::explicit_push(
    b"[url \"ssh://ignored/\"]\npushInsteadOf=ssh://HOST/\n[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit\npushurl=ssh://[PUSH]/repo%2Egit",
    b"HOST", b"PUSH", b"ssh://PUSH/repo%2Egit"
)]
#[case::explicit_push_rewrite(
    b"[url \"custom://[PUSH]/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit\npushurl=alias:repo%2Egit",
    b"HOST", b"[PUSH]", b"custom://%5BPUSH%5D/repo%2Egit"
)]
#[case::invalid_push_rewrite(
    b"[url \"host:\"]\npushInsteadOf=ssh://HOST/repo%2Egit\n[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit",
    b"HOST", b"HOST", b"ssh://HOST/repo%2Egit"
)]
#[case::invalid_push_rewrite_with_ordinary(
    b"[url \"ssh://[FETCH]/\"]\ninsteadOf=ssh://HOST/\n[url \"host:\"]\npushInsteadOf=ssh://HOST/repo%2Egit\n[remote \"origin\"]\nurl=ssh://[HOST]/repo%2Egit",
    b"FETCH", b"HOST", b"ssh://HOST/repo%2Egit"
)]
fn selected_push_parts_follow_the_same_rewrite_as_bytes(
    #[case] body: &[u8],
    #[case] fetch_host: &[u8],
    #[case] push_host: &[u8],
    #[case] push_bytes: &[u8],
) {
    let remote = find(body).unwrap().unwrap();
    assert_eq!(remote.fetch_url_parts().unwrap().host(), Some(fetch_host));
    assert_eq!(remote.push_url(), Some(push_bytes));
    assert_eq!(remote.push_url_parts().unwrap().host(), Some(push_host));
    assert_eq!(remote.push_url_parts().unwrap().path(), b"/repo.git");
}

#[test]
fn push_only_parts_do_not_create_a_fetch_url() {
    let remote = find(b"[remote \"origin\"]\npushurl=ssh://[PUSH]/repo%2Egit")
        .unwrap()
        .unwrap();
    assert_eq!(remote.fetch_url(), None);
    assert_eq!(remote.fetch_url_parts(), None);
    assert_eq!(
        remote.push_url_parts().unwrap().host(),
        Some(b"PUSH".as_slice())
    );
}

#[test]
fn retaining_selected_parts_does_not_skip_later_validation() {
    let error = find(b"[remote \"origin\"]\nurl=ssh://[HOST]/repo\nurl=https://host:bad/repo\npushurl=ssh://[PUSH]/repo").unwrap_err();
    assert_eq!(
        error,
        ConfiguredRemoteError::Url {
            key: "url",
            occurrence: 2,
            rewritten: false,
            source: ConfiguredUrlError::Port,
        }
    );
}

// Original public remote lookup and installed Git select different destinations when SSH IPv6
// brackets participate in canonical prefix matching. Require whole-remote compatibility here.
#[rstest]
#[case::witness(b"[url \"ssh://[FETCH]/repo\"]\ninsteadOf=ssh://[::1]/repo%2Egit\n[url \"host:\"]\npushInsteadOf=ssh://[::1]/repo%2Egit\n")]
#[case::ordinary_nonmatching(b"[url \"ssh://other/\"]\ninsteadOf=unrelated:\n")]
#[case::push_nonmatching(b"[url \"ssh://other/\"]\npushInsteadOf=unrelated:\n")]
fn ssh_ipv6_original_with_rewrite_requires_compatibility(#[case] rules: &[u8]) {
    let body = [rules, b"[remote \"origin\"]\nurl=ssh://[::1]/repo%2Egit\n"].concat();
    assert_eq!(
        find(&body).unwrap_err(),
        ConfiguredRemoteError::UnsupportedUrlSyntax {
            key: "url",
            occurrence: 1,
            rewritten: true,
        }
    );
}

#[rstest]
#[case::no_rewrite(b"[remote \"origin\"]\nurl=ssh://[::1]/repo%2Egit\n")]
#[case::replacement(
    b"[url \"ssh://[::1]/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=alias:repo%2Egit\n"
)]
#[case::explicit_push_ignores_push_rules(b"[url \"ssh://other/\"]\npushInsteadOf=ssh://\n[remote \"origin\"]\nurl=ssh://[::1]/repo%2Egit\npushurl=ssh://[::1]/repo%2Egit\n")]
fn ssh_ipv6_without_ambiguous_prefix_matching_retains_native_parts(#[case] body: &[u8]) {
    let remote = find(body).unwrap().unwrap();
    assert_eq!(
        remote.fetch_url(),
        Some(b"ssh://[::1]/repo%2Egit".as_slice())
    );
    assert_eq!(
        remote.fetch_url_parts().unwrap().host(),
        Some(b"::1".as_slice())
    );
    assert_eq!(remote.fetch_url_parts().unwrap().path(), b"/repo.git");
    assert_eq!(remote.push_url_parts(), remote.fetch_url_parts());
}

#[test]
fn ssh_ipv6_guard_preserves_earlier_rewrite_failure() {
    let error = find(b"[url \"https://host:bad/\"]\ninsteadOf=alias:\n[remote \"origin\"]\nurl=alias:repo\nurl=ssh://[::1]/repo\n").unwrap_err();
    assert_eq!(
        error,
        ConfiguredRemoteError::Url {
            key: "url",
            occurrence: 1,
            rewritten: true,
            source: ConfiguredUrlError::Port,
        }
    );
}

#[test]
fn standalone_ipv6_rewrite_guard_uses_only_ordinary_rules() {
    let ordinary = Config::parse(b"[url \"ssh://other/\"]\ninsteadOf=unrelated:\n").unwrap();
    assert_eq!(
        super::rewrite_configured_url(&ordinary, b"ssh://[::1]/repo"),
        Err(ConfiguredUrlError::Unsupported)
    );
    let push = Config::parse(b"[url \"ssh://other/\"]\npushInsteadOf=ssh://\n").unwrap();
    assert_eq!(
        super::rewrite_configured_url(&push, b"ssh://[::1]/repo").unwrap(),
        b"ssh://[::1]/repo"
    );
}
