use rstest::rstest;

use super::*;

#[rstest]
#[case::first_and_all(b"url=one\nurl=two\n", Some(b"one".as_slice()), vec![b"one".as_slice(), b"two"])]
#[case::push_override(b"url=one\npushurl=p\npushurl=q\n", Some(b"one".as_slice()), vec![b"p".as_slice(), b"q"])]
#[case::url_reset(b"url=old\nurl=\nurl=new\n", Some(b"new".as_slice()), vec![b"new".as_slice()])]
#[case::push_reset_fallback(b"url=one\npushurl=old\npushurl=\n", Some(b"one".as_slice()), vec![b"one".as_slice()])]
#[case::reset_then_append(b"url=one\npushurl=old\npushurl=\npushurl=new\n", Some(b"one".as_slice()), vec![b"new".as_slice()])]
#[case::all_reset(b"url=old\nurl=\n", None, vec![])]
#[case::push_only(b"pushurl=p\n", None, vec![b"p".as_slice()])]
#[case::duplicate(b"url=a\nurl=a\n", Some(b"a".as_slice()), vec![b"a".as_slice(), b"a"])]
#[case::bytes(b"url=\xff\n", Some(b"\xff".as_slice()), vec![b"\xff".as_slice()])]
#[case::missing_urls(b"fetch=refs/heads/main\n", None, vec![])]
fn url_order_and_fallback(
    #[case] entries: &[u8],
    #[case] fetch: Option<&[u8]>,
    #[case] push: Vec<&[u8]>,
) {
    let bytes = [b"[remote \"origin\"]\n".as_slice(), entries].concat();
    let config = Config::parse(&bytes).unwrap();
    let remote = Remote::find(&config, b"origin").unwrap().unwrap();
    assert_eq!(remote.fetch_url(), fetch);
    assert_eq!(remote.push_urls(), push);
}

#[rstest]
#[case::url(b"url\n", "url")]
#[case::pushurl(b"pushurl\n", "pushurl")]
#[case::fetch(b"fetch\n", "fetch")]
#[case::push(b"push\n", "push")]
fn rejects_implicit_values(#[case] entry: &[u8], #[case] key: &str) {
    let bytes = [b"[remote \"o\"]\n".as_slice(), entry].concat();
    let config = Config::parse(&bytes).unwrap();
    let error = Remote::find(&config, b"o").unwrap_err();
    assert!(
        matches!(error, RemoteError::MissingValue { key: actual, occurrence: 1 } if key == actual)
    );
}

#[rstest]
#[case::empty("fetch=", "fetch", 1)]
#[case::second("fetch=refs/heads/main\nfetch=main", "fetch", 2)]
#[case::other_direction("url=valid\nfetch=refs/heads/main\npush=^refs/heads/main", "push", 1)]
fn diagnoses_bad_refspec_occurrences(
    #[case] entries: &str,
    #[case] key: &str,
    #[case] occurrence: usize,
) {
    let config = Config::parse(format!("[remote \"o\"]\n{entries}\n").as_bytes()).unwrap();
    let error = Remote::find(&config, b"o").unwrap_err();
    assert!(
        matches!(error, RemoteError::Refspec { key: actual, occurrence: index, .. } if actual == key && index == occurrence)
    );
}

#[test]
fn remote_names_and_repeated_sections_are_exact() {
    let config = Config::parse(b"[remote \"Origin\"]\nURL=first\nfetch=refs/heads/main\n[remote \"origin\"]\nunknown=yes\n[REMOTE \"Origin\"]\nurl=second\nfetch=refs/heads/main\n[remote \"\xff\"]\nunknown=yes\n[remote \"empty\"]\n").unwrap();
    assert_eq!(
        Remote::names(&config),
        vec![b"Origin".as_slice(), b"origin", b"\xff"]
    );
    let remote = Remote::find(&config, b"Origin").unwrap().unwrap();
    assert_eq!(remote.name(), b"Origin");
    assert_eq!(remote.urls(), vec![b"first".to_vec(), b"second".to_vec()]);
    assert_eq!(remote.fetch_refspecs().specs().len(), 2);
    assert!(remote.push_refspecs().specs().is_empty());
    assert!(Remote::find(&config, b"missing").unwrap().is_none());
    assert!(
        Remote::find(&config, b"origin")
            .unwrap()
            .unwrap()
            .urls()
            .is_empty()
    );
}
