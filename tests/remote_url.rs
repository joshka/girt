//! Public URL presentation behavior against an independently created Git repository.

#[cfg(unix)]
#[test]
fn local_and_file_urls_open_with_git() {
    use std::process::Command;

    use girt::remote::ParsedUrl;

    let fixture = tempfile::tempdir().unwrap();
    let repo = fixture.path().join("a b.git");
    assert!(
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&repo)
            .output()
            .unwrap()
            .status
            .success()
    );

    let local = ParsedUrl::parse(b"a b.git").unwrap();
    let absolute = local.canonicalize_local(fixture.path()).unwrap();
    assert_eq!(absolute, repo.as_os_str().as_encoded_bytes());
    assert!(
        Command::new("git")
            .args(["ls-remote"])
            .arg(std::str::from_utf8(&absolute).unwrap())
            .output()
            .unwrap()
            .status
            .success()
    );

    let file_url = format!("file://{}", repo.to_str().unwrap().replace(' ', "%20"));
    let parsed = ParsedUrl::parse(file_url.as_bytes()).unwrap();
    assert_eq!(
        parsed.canonicalize_local(fixture.path()).unwrap(),
        file_url.as_bytes()
    );
    assert!(
        Command::new("git")
            .args(["ls-remote", &file_url])
            .output()
            .unwrap()
            .status
            .success()
    );
}
