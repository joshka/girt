use rstest::rstest;

use super::*;

#[rstest]
#[case::option("-oProxyCommand=secret", "user", "repo")]
#[case::url("ssh://host", "user", "repo")]
#[case::scp("user@host:repo", "user", "repo")]
#[case::user("host", "-secret", "repo")]
#[case::path_option("host", "user", "--secret")]
#[case::tilde("host", "user", "~/secret")]
#[case::newline("host", "user", "repo\nsecret")]
#[case::empty("", "user", "repo")]
fn rejects_unsafe_components(#[case] host: &str, #[case] user: &str, #[case] path: &str) {
    let error = SshRemote::new(
        host,
        user,
        22,
        path,
        Path::new("/usr/bin/ssh"),
        Path::new("/dev/null"),
    )
    .unwrap_err();
    assert!(matches!(error, SshError::Configuration(_)));
    assert!(!format!("{error:?} {error}").contains("secret"));
}
#[test]
fn quotes_path_and_separates_options() {
    let remote = SshRemote::new(
        "::1",
        "user",
        22,
        "repo ' ; $(secret)",
        Path::new("/usr/bin/ssh"),
        Path::new("/dev/null"),
    )
    .unwrap();
    let command = remote.command("git-upload-pack");
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(
        *args.last().unwrap(),
        "git-upload-pack 'repo '\\'' ; $(secret)'"
    );
    assert_eq!(args[args.len() - 3], "--");
    assert!(!format!("{remote:?}").contains("secret"));
}
#[rstest]
#[case::zero_port(0, "/usr/bin/ssh", "/dev/null")]
#[case::relative_executable(22, "ssh", "/dev/null")]
#[case::relative_config(22, "/usr/bin/ssh", "config")]
fn rejects_unsupported_configuration(#[case] port: u16, #[case] exe: &str, #[case] config: &str) {
    assert!(
        SshRemote::new(
            "host",
            "user",
            port,
            "repo",
            Path::new(exe),
            Path::new(config)
        )
        .is_err()
    );
}
