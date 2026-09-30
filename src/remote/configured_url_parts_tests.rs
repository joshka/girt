//! Original public gix 0.87.1 component observations; no upstream implementation or tests used.

use rstest::rstest;

use super::{ConfiguredUrlError, normalize_configured_url, parse_configured_url};
use crate::remote::Protocol;

#[rstest]
#[case::local(b"./repo%20name", Protocol::Local, None, b"./repo%20name")]
#[case::local_bytes(b"./repo/\xff", Protocol::Local, None, b"./repo/\xff")]
#[case::file(b"file:///tmp/repo%2Egit", Protocol::File, None, b"/tmp/repo%2Egit")]
#[case::file_host(b"file://HOST/tmp/repo%2Egit", Protocol::File, Some(b"HOST".as_slice()), b"/tmp/repo%2Egit")]
#[case::file_spaces(b"file:///tmp/repo  ", Protocol::File, None, b"/tmp/repo  ")]
#[case::http_path(b"https://HOST/repo%2Egit", Protocol::Https, Some(b"host".as_slice()), b"/repo.git")]
#[case::http_slashes(b"https://HOST/%2Forg%2Frepo.git", Protocol::Https, Some(b"host".as_slice()), b"//org/repo.git")]
#[case::http_percent(b"https://host/%25.git", Protocol::Https, Some(b"host".as_slice()), b"/%.git")]
#[case::http_utf8(b"https://host/%C3%A9.git", Protocol::Https, Some(b"host".as_slice()), b"/\xc3\xa9.git")]
#[case::http_raw_utf8(b"https://host/\xc3\xa9.git", Protocol::Https, Some(b"host".as_slice()), b"/\xc3\xa9.git")]
#[case::http_nul(b"https://host/a%00b", Protocol::Https, Some(b"host".as_slice()), b"/a\0b")]
#[case::http_query(b"https://HOST/a%2fb%2Egit?x%20z#frag", Protocol::Https, Some(b"host".as_slice()), b"/a/b.git?x z#frag")]
#[case::http_query_only(b"https://host?x%20y", Protocol::Https, Some(b"host".as_slice()), b"?x y")]
#[case::http_empty_path(b"https://host", Protocol::Https, Some(b"host".as_slice()), b"/")]
#[case::http_encoded_host(b"https://HOST%2Eexample/a", Protocol::Https, Some(b"HOST%2Eexample".as_slice()), b"/a")]
#[case::http_ipv6(b"https://[::1]/repo", Protocol::Https, Some(b"[::1]".as_slice()), b"/repo")]
#[case::credentials(b"https://user:secret@HOST:80/repo", Protocol::Https, Some(b"host".as_slice()), b"/repo")]
#[case::ssh_path(b"ssh://HOST/repo%2Egit", Protocol::Ssh, Some(b"host".as_slice()), b"/repo.git")]
#[case::ssh_encoded_host(b"ssh://HOST%2Eexample/a", Protocol::Ssh, Some(b"host.example".as_slice()), b"/a")]
#[case::ssh_opaque_host(b"ssh://HOST%3Fx/r", Protocol::Ssh, Some(b"HOST?x".as_slice()), b"/r")]
#[case::ssh_ipv6(b"ssh://[::1]/repo", Protocol::Ssh, Some(b"::1".as_slice()), b"/repo")]
#[case::ssh_bracketed_name(b"ssh://[host]/repo", Protocol::Ssh, Some(b"host".as_slice()), b"/repo")]
#[case::ssh_empty_brackets(b"ssh://[]/repo", Protocol::Ssh, Some(b"".as_slice()), b"/repo")]
#[case::scp(b"git@HOST:org/repo%2Egit", Protocol::Ssh, Some(b"host".as_slice()), b"org/repo%2Egit")]
#[case::scp_ipv6(b"[::1]:repo", Protocol::Ssh, Some(b"::1".as_slice()), b"repo")]
#[case::scp_encoded_host(b"HOST%2Eexample:repo", Protocol::Ssh, Some(b"HOST%2Eexample".as_slice()), b"repo")]
#[case::scp_unicode_host(b"M\xc3\xbcnich:repo", Protocol::Ssh, Some(b"M\xc3\xbcnich".as_slice()), b"repo")]
#[case::git(b"git://HOST/repo%2Egit", Protocol::Git, Some(b"host".as_slice()), b"/repo.git")]
#[case::custom(b"custom://HOST/repo%2Egit", Protocol::Helper, Some(b"host".as_slice()), b"/repo.git")]
#[case::custom_encoded_host(b"custom://HOST%2Eexample/a", Protocol::Helper, Some(b"host.example".as_slice()), b"/a")]
#[case::custom_opaque_host(b"custom://HOST?x/r", Protocol::Helper, Some(b"HOST?x".as_slice()), b"/r")]
#[case::custom_bracket_host(b"custom://[::1]/repo", Protocol::Helper, Some(b"[::1]".as_slice()), b"/repo")]
#[case::custom_unicode_host(b"custom://M\xc3\xbcnich/repo", Protocol::Helper, Some(b"M\xc3\xbcnich".as_slice()), b"/repo")]
#[case::custom_hostless(b"custom:///repo", Protocol::Helper, None, b"/repo")]
#[case::custom_empty_path(b"custom://HOST", Protocol::Helper, Some(b"host".as_slice()), b"")]
#[case::uppercase_http(b"HTTPS://HOST/repo%2Egit", Protocol::Helper, Some(b"host".as_slice()), b"/repo.git")]
#[case::uppercase_http_empty(b"HTTPS://HOST", Protocol::Helper, Some(b"host".as_slice()), b"")]
#[case::uppercase_file(b"FILE:///repo%2Egit", Protocol::Helper, None, b"/repo.git")]
fn presentation_components(
    #[case] input: &[u8],
    #[case] protocol: Protocol,
    #[case] host: Option<&[u8]>,
    #[case] path: &[u8],
) {
    let parts = parse_configured_url(input).unwrap();
    assert_eq!(parts.protocol(), protocol);
    assert_eq!(parts.host(), host);
    assert_eq!(parts.path(), path);
}

#[rstest]
#[case::http(
    b"https://HOST%2Eexample/r",
    b"https://HOST%2Eexample/r",
    b"HOST%2Eexample"
)]
#[case::ssh(b"ssh://HOST%2Eexample/r", b"ssh://host.example/r", b"host.example")]
#[case::custom(
    b"custom://HOST%2Eexample/r",
    b"custom://host.example/r",
    b"host.example"
)]
#[case::ssh_percent(b"ssh://host%25name/r", b"ssh://host%25name/r", b"host%name")]
#[case::custom_percent(b"custom://host%25name/r", b"custom://host%25name/r", b"host%name")]
#[case::ssh_escaped_separator(b"ssh://HOST%3Aname/r", b"ssh://HOST%3Aname/r", b"HOST:name")]
#[case::scp_bracket_case(b"[HOST]:repo", b"HOST:repo", b"HOST")]
#[case::ssh_bracket_case(b"ssh://[HOST]/repo", b"ssh://HOST/repo", b"HOST")]
#[case::scp_unicode_case(b"M\xc3\xbcnich:repo", b"M\xc3\xbcnich:repo", b"M\xc3\xbcnich")]
fn host_interpretation_and_serialization_share_protocol_rules(
    #[case] input: &[u8],
    #[case] serialized: &[u8],
    #[case] host: &[u8],
) {
    assert_eq!(normalize_configured_url(input).unwrap(), serialized);
    assert_eq!(parse_configured_url(input).unwrap().host(), Some(host));
}

#[test]
fn ipv6_components_retain_original_meaning_with_corrected_serialization() {
    // The observed reference serializer drops these brackets. The native serializer intentionally
    // retains valid IPv6 authority syntax while exposing the same unbracketed SSH host component.
    let input = b"ssh://[2001:db8:0:0:0:0:0:1]/repo";
    assert_eq!(normalize_configured_url(input).unwrap(), input);
    assert_eq!(
        parse_configured_url(input).unwrap().host(),
        Some(b"2001:db8:0:0:0:0:0:1".as_slice())
    );
}

#[rstest]
#[case::empty(b"", ConfiguredUrlError::MissingPath)]
#[case::missing_host(b"https:///repo", ConfiguredUrlError::Authority)]
#[case::invalid_port(b"https://user:secret@host:bad/repo", ConfiguredUrlError::Port)]
#[case::invalid_escape(b"https://user:secret@host/%ZZ", ConfiguredUrlError::PathEscape)]
#[case::decoded_utf8(b"https://host/%FF", ConfiguredUrlError::PathEscape)]
#[case::raw_utf8(b"https://host/\xff", ConfiguredUrlError::Encoding)]
#[case::helper(b"helper::repo", ConfiguredUrlError::Unsupported)]
#[case::windows(b"C:/repo", ConfiguredUrlError::Unsupported)]
fn parts_and_serialization_share_validation(
    #[case] input: &[u8],
    #[case] expected: ConfiguredUrlError,
) {
    let error = parse_configured_url(input).unwrap_err();
    assert_eq!(error, expected);
    assert_eq!(normalize_configured_url(input), Err(expected));
    assert!(!format!("{error:?}: {error}").contains("secret"));
}

#[test]
fn debug_omits_credentials_hosts_and_paths() {
    let parts = parse_configured_url(b"https://user:secret@private.example/private-path").unwrap();
    let debug = format!("{parts:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("private"));
}

#[cfg(unix)]
#[test]
fn local_components_do_not_follow_filesystem_links() {
    let directory = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("loop", directory.path().join("loop")).unwrap();
    let input = format!("{}/loop/../repo", directory.path().display());
    let parts = parse_configured_url(input.as_bytes()).unwrap();
    assert_eq!(parts.protocol(), Protocol::Local);
    assert_eq!(parts.host(), None);
    assert_eq!(parts.path(), input.as_bytes());
    assert_eq!(
        std::fs::read_link(directory.path().join("loop")).unwrap(),
        std::path::Path::new("loop")
    );
}

#[rstest]
#[case::http("http", "[abcd::1]", b"[abcd::1]")]
#[case::https("https", "[abcd::1]", b"[abcd::1]")]
#[case::ssh("ssh", "[abcd::1]", b"abcd::1")]
#[case::git("git", "%5Babcd%3A%3A1%5D", b"[abcd::1]")]
#[case::custom("custom", "%5Babcd%3A%3A1%5D", b"[abcd::1]")]
#[case::mixed_case("HTTPS", "%5Babcd%3A%3A1%5D", b"[abcd::1]")]
fn ipv6_case_and_brackets_follow_scheme_rules(
    #[case] scheme: &str,
    #[case] rendered_host: &str,
    #[case] host: &[u8],
) {
    let input = format!("{scheme}://u%41:p%3a@[ABCD::1]/repo%2Egit");
    let expected = format!("{scheme}://uA:p:@{rendered_host}/repo%2Egit");
    let parts = parse_configured_url(input.as_bytes()).unwrap();
    assert_eq!(parts.host(), Some(host));
    assert_eq!(parts.path(), b"/repo.git");
    assert_eq!(
        normalize_configured_url(input.as_bytes()).unwrap(),
        expected.as_bytes()
    );
}
