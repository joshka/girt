//! Original mailmap corpus compared with the installed Git executable.
#[path = "support/history_git.rs"]
#[allow(dead_code)]
mod oracle;
use girt::mailmap::Mailmap;

#[test]
fn check_mailmap_corpus() {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "--template="], b"");
    let sources: &[&[u8]] = &[
        b"Canon <old@e>\n",
        b"<new@e> <old@e>\n",
        b"Canon <new@e> <old@e>\n",
        b"Canon <new@e> Old <old@e>\n",
        b"One <new@e> <old@e>\nTwo <old@e>\n",
        b"Generic <generic@e> <old@e>\nSpecific <specific@e> Old <old@e>\n",
        b"Specific <specific@e> Old <old@e>\nGeneric <generic@e> <old@e>\n",
        b"# comment\n\nmalformed\n  Canon <new@e> <old@e> # trailing comment\r\n",
        b"Canon <new@e> Old <old@e>\nOther <other@e> OLD <OLD@E>\n",
    ];
    for &source in sources {
        std::fs::write(root.path().join(".mailmap"), source).unwrap();
        let map = Mailmap::parse(source);
        for name in [b"Old".as_slice(), b"old", b"Other", b"Unknown"] {
            for email in [b"old@e".as_slice(), b"OLD@E", b"unknown@e"] {
                let mut input = name.to_vec();
                input.extend_from_slice(b" <");
                input.extend_from_slice(email);
                input.extend_from_slice(b">\n");
                let expected = oracle::git(root.path(), &["check-mailmap", "--stdin"], &input);
                let actual = map.resolve(name, email);
                let mut output = actual.name.to_vec();
                output.extend_from_slice(b" <");
                output.extend_from_slice(actual.email);
                output.extend_from_slice(b">\n");
                assert_eq!(output, expected, "source {source:?}, input {input:?}");
            }
        }
    }
}

#[test]
fn repository_source_precedence_and_bare_default() {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "--template="], b"");
    let work = b"Work <work@e> <old@e>\n";
    let blob = b"Blob <blob@e> <old@e>\n";
    let file = b"File <file@e> <old@e>\n";
    std::fs::write(root.path().join(".mailmap"), work).unwrap();
    std::fs::write(root.path().join("extra"), file).unwrap();
    let id = oracle::git(root.path(), &["hash-object", "-w", "--stdin"], blob);
    let id = std::str::from_utf8(&id).unwrap().trim();
    oracle::git(root.path(), &["config", "mailmap.blob", id], b"");
    for configured_file in [false, true] {
        if configured_file {
            oracle::git(root.path(), &["config", "mailmap.file", "extra"], b"");
        }
        let repository = girt::Repository::open(root.path()).unwrap();
        let map = Mailmap::load(&repository, &repository.objects().unwrap()).unwrap();
        let expected = oracle::git(root.path(), &["check-mailmap", "Old <old@e>"], b"");
        let identity = map.resolve(b"Old", b"old@e");
        assert_eq!(
            format!(
                "{} <{}>\n",
                String::from_utf8_lossy(identity.name),
                String::from_utf8_lossy(identity.email)
            )
            .as_bytes(),
            expected
        );
    }
    oracle::git(root.path(), &["add", ".mailmap"], b"");
    oracle::git(root.path(), &["commit", "-m", "mailmap"], b"");
    let bare = tempfile::tempdir().unwrap();
    oracle::git(
        bare.path(),
        &["clone", "--bare", root.path().to_str().unwrap(), "."],
        b"",
    );
    let repository = girt::Repository::open(bare.path()).unwrap();
    let map = Mailmap::load(&repository, &repository.objects().unwrap()).unwrap();
    assert_eq!(map.resolve(b"Old", b"old@e").name, b"Work");
    assert_eq!(
        oracle::git(bare.path(), &["check-mailmap", "Old <old@e>"], b""),
        b"Work <work@e>\n"
    );
}
