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
#[case::tag_cleared(
    b"[remote \"origin\"]\ntagOpt=bad\ntagOpt\nurl=good",
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
#[case::cleared(b"tagOpt=bad\ntagOpt", None)]
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
