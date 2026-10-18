//! Original fixtures, compared only with the Git executable (no implementation source).
#[path = "support/layout_git.rs"]
mod oracle;

use girt::submodule::{Error, FetchRecurse, Ignore, Modules, Update};
use oracle::{attempt, git};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"], b"");
    git(
        dir.path(),
        &["commit", "--allow-empty", "-qm", "initial"],
        b"",
    );
    let id = git(dir.path(), &["rev-parse", "HEAD"], b"");
    let entry = format!("160000,{},vendor/lib", String::from_utf8_lossy(&id).trim());
    git(
        dir.path(),
        &["update-index", "--add", "--cacheinfo", &entry],
        b"",
    );
    git(
        dir.path(),
        &[
            "config",
            "remote.origin.url",
            "https://example.com/team/parent.git",
        ],
        b"",
    );
    dir
}

#[test]
fn settings_and_scalar_precedence_match_git() {
    let dir = fixture();
    let bytes = b"[include]\npath = absent\n[submodule \"logical\"]\npath = unused\npath = vendor/lib\nurl = old\nurl = ../child.git\nbranch = .\nupdate = rebase\nignore = untracked\nshallow\nfetchRecurseSubmodules = on-demand\n";
    std::fs::write(dir.path().join(".gitmodules"), bytes).unwrap();
    let modules = Modules::parse(bytes).unwrap();
    let definition = modules.get(b"logical").unwrap();
    definition.validate().unwrap();
    assert_eq!(definition.name(), b"logical");
    assert_eq!(definition.path(), Some(b"vendor/lib".as_slice()));
    assert_eq!(definition.url(), Some(b"../child.git".as_slice()));
    assert_eq!(definition.branch(), Some(b".".as_slice()));
    assert_eq!(definition.update(), Update::Rebase);
    assert_eq!(definition.ignore(), Ignore::Untracked);
    assert_eq!(definition.shallow(), Some(true));
    assert_eq!(definition.fetch_recurse(), Some(FetchRecurse::OnDemand));
    git(dir.path(), &["submodule", "init"], b"");
    assert_eq!(
        git(
            dir.path(),
            &["config", "--get", "submodule.logical.url"],
            b""
        ),
        b"https://example.com/team/child.git\n"
    );
    assert_eq!(
        git(
            dir.path(),
            &["config", "--get", "submodule.logical.update"],
            b""
        ),
        b"rebase\n"
    );
}

#[test]
fn command_updates_and_bad_typed_settings_are_refused() {
    let dir = fixture();
    for setting in [
        "update = !echo untrusted",
        "update = unknown",
        "shallow = unknown",
        "fetchRecurseSubmodules = unknown",
    ] {
        let bytes =
            format!("[submodule \"logical\"]\npath = vendor/lib\nurl = ../child.git\n{setting}\n");
        std::fs::write(dir.path().join(".gitmodules"), &bytes).unwrap();
        assert!(Modules::parse(bytes.as_bytes()).is_err(), "{setting}");
        assert!(
            !attempt(dir.path(), &["submodule", "init"], b"")
                .status
                .success(),
            "{setting}"
        );
    }
}

#[test]
fn invalid_ignore_falls_back_as_git_does() {
    let dir = fixture();
    let bytes =
        b"[submodule \"logical\"]\npath = vendor/lib\nurl = ../child.git\nignore = unknown\n";
    std::fs::write(dir.path().join(".gitmodules"), bytes).unwrap();
    assert_eq!(
        Modules::parse(bytes)
            .unwrap()
            .get(b"logical")
            .unwrap()
            .ignore(),
        Ignore::Unspecified
    );
    let output = attempt(dir.path(), &["submodule", "init"], b"");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid parameter"));
}

#[test]
fn traversal_names_are_inspectable_but_not_operational() {
    let dir = fixture();
    for name in ["../evil", "nested/../evil"] {
        let bytes = format!("[submodule \"{name}\"]\npath = vendor/lib\nurl = ../child.git\n");
        std::fs::write(dir.path().join(".gitmodules"), &bytes).unwrap();
        let modules = Modules::parse(bytes.as_bytes()).unwrap();
        assert!(matches!(
            modules.get(name.as_bytes()).unwrap().validate(),
            Err(Error::UnsafeName)
        ));
        let output = attempt(dir.path(), &["submodule", "init"], b"");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("suspicious submodule name"));
    }
}

#[test]
fn lexical_validation_refuses_portable_escape_and_option_forms() {
    for path in [
        "",
        "/absolute",
        "../escape",
        "a/../b",
        "a//b",
        "a/./b",
        "a/.GIT/b",
        "C:/escape",
        "a\\b",
        "-option",
        "a\tbad",
    ] {
        let escaped = path.replace('\\', "\\\\").replace('\t', "\\t");
        let bytes = format!("[submodule \"safe\"]\npath = \"{escaped}\"\nurl = ../child.git\n");
        let modules = Modules::parse(bytes.as_bytes()).unwrap();
        assert!(
            matches!(
                modules.get(b"safe").unwrap().validate(),
                Err(Error::UnsafePath)
            ),
            "{path:?}"
        );
    }
    for url in ["", "-upload-pack=evil", "bad\\nurl"] {
        let bytes = format!("[submodule \"safe\"]\npath = safe\nurl = \"{url}\"\n");
        let modules = Modules::parse(bytes.as_bytes()).unwrap();
        assert!(matches!(
            modules.get(b"safe").unwrap().validate(),
            Err(Error::UnsafeUrl)
        ));
    }
}

#[test]
fn bytes_defaults_empty_sections_and_case_sensitive_names() {
    let modules = Modules::parse(b"[submodule \"A\"]\npath = a\nurl = child\n[submodule \"a\"]\n[submodule \"raw\"]\npath = raw\xff\nurl = ../raw\xff\n").unwrap();
    assert_eq!(modules.definitions().len(), 3);
    assert_eq!(modules.get(b"A").unwrap().update(), Update::Checkout);
    assert_eq!(modules.get(b"a").unwrap().path(), None);
    assert!(matches!(
        modules.get(b"a").unwrap().validate(),
        Err(Error::UnsafePath)
    ));
    assert_eq!(
        modules.get(b"raw").unwrap().path(),
        Some(b"raw\xff".as_slice())
    );
    modules.get(b"raw").unwrap().validate().unwrap();
}

#[test]
fn implicit_strings_and_shadowed_invalid_assignments_match_git_refusal() {
    let dir = fixture();
    for setting in [
        "path",
        "url",
        "branch",
        "update",
        "ignore",
        "update = !echo unsafe\nupdate = checkout",
        "shallow = invalid\nshallow = true",
    ] {
        let bytes = format!(
            "[submodule \"logical\"]\npath = vendor/lib\nurl = ../child.git\nupdate = merge\n{setting}\n"
        );
        std::fs::write(dir.path().join(".gitmodules"), &bytes).unwrap();
        assert!(
            matches!(
                Modules::parse(bytes.as_bytes()),
                Err(Error::InvalidSetting { .. })
            ),
            "{setting}"
        );
        assert!(
            !attempt(dir.path(), &["submodule", "init"], b"")
                .status
                .success(),
            "{setting}"
        );
    }
}

#[test]
fn repository_sources_select_worktree_index_and_committed_bytes() {
    use girt::submodule::Source;
    for format in ["sha1", "sha256"] {
        let dir = tempfile::tempdir().unwrap();
        git(
            dir.path(),
            &["init", "-q", &format!("--object-format={format}")],
            b"",
        );
        let path = dir.path().join(".gitmodules");
        let definition = |url| format!("[submodule \"logical\"]\npath = vendor/lib\nurl = {url}\n");
        std::fs::write(&path, definition("committed")).unwrap();
        git(dir.path(), &["add", ".gitmodules"], b"");
        git(dir.path(), &["commit", "-qm", "modules"], b"");
        let head = git(dir.path(), &["rev-parse", "HEAD"], b"");
        let head = String::from_utf8_lossy(&head).trim().parse().unwrap();
        let cacheinfo = format!("160000,{head},vendor/lib");
        git(
            dir.path(),
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
            b"",
        );
        std::fs::write(&path, definition("indexed")).unwrap();
        git(dir.path(), &["add", ".gitmodules"], b"");
        std::fs::write(&path, definition("worktree")).unwrap();
        let repo = girt::Repository::open(dir.path()).unwrap();
        let objects = repo.objects().unwrap();
        for (source, expected) in [
            (Source::Worktree, "worktree"),
            (Source::WorktreeOrIndex, "worktree"),
            (Source::Index, "indexed"),
            (Source::Commit(head), "committed"),
        ] {
            let loaded = Modules::read(&repo, &objects, source).unwrap().unwrap();
            assert_eq!(
                loaded.get(b"logical").unwrap().url(),
                Some(expected.as_bytes())
            );
        }
        git(dir.path(), &["submodule", "init"], b"");
        assert_eq!(
            git(
                dir.path(),
                &["config", "--get", "submodule.logical.url"],
                b""
            ),
            b"worktree\n"
        );
        git(
            dir.path(),
            &["config", "--remove-section", "submodule.logical"],
            b"",
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            Modules::read(&repo, &objects, Source::Worktree)
                .unwrap()
                .is_none()
        );
        let loaded = Modules::read(&repo, &objects, Source::WorktreeOrIndex)
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.get(b"logical").unwrap().url(),
            Some(b"indexed".as_slice())
        );
        git(dir.path(), &["submodule", "init"], b"");
        assert_eq!(
            git(
                dir.path(),
                &["config", "--get", "submodule.logical.url"],
                b""
            ),
            b"indexed\n"
        );
        git(
            dir.path(),
            &["update-index", "--force-remove", ".gitmodules"],
            b"",
        );
        assert!(
            Modules::read(&repo, &objects, Source::WorktreeOrIndex)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn source_limits_conflicts_and_nonregular_entries_are_explicit() {
    use girt::submodule::{LoadError, Source};
    let dir = fixture();
    let path = dir.path().join(".gitmodules");
    let bytes = b"[submodule \"logical\"]\npath = vendor/lib\nurl = child\n";
    std::fs::write(&path, bytes).unwrap();
    git(dir.path(), &["add", ".gitmodules"], b"");
    let repo = girt::Repository::open(dir.path()).unwrap();
    let objects = repo.objects().unwrap();
    assert!(matches!(
        Modules::read_with(&repo, &objects, Source::Worktree, 1),
        Err(LoadError::Limit)
    ));
    assert!(matches!(
        Modules::read_with(&repo, &objects, Source::Index, 1),
        Err(LoadError::Blob(_))
    ));
    let id = girt::ObjectId::for_blob(repo.object_format(), bytes);
    std::fs::remove_file(&path).unwrap();
    git(
        dir.path(),
        &["update-index", "--force-remove", ".gitmodules"],
        b"",
    );
    let conflict = format!("100644 {id} 2\t.gitmodules\n100644 {id} 3\t.gitmodules\n");
    git(
        dir.path(),
        &["update-index", "--index-info"],
        conflict.as_bytes(),
    );
    assert!(matches!(
        Modules::read(&repo, &objects, Source::WorktreeOrIndex),
        Err(LoadError::Conflict)
    ));
    git(
        dir.path(),
        &["update-index", "--force-remove", ".gitmodules"],
        b"",
    );
    let symlink = format!("120000,{id},.gitmodules");
    assert!(
        !attempt(
            dir.path(),
            &["update-index", "--add", "--cacheinfo", &symlink],
            b""
        )
        .status
        .success()
    );
    // Git refuses constructing this malicious index entry; exercise imported data independently.
    let mut edit = repo.edit_index(Default::default()).unwrap();
    edit.replace_entries(vec![girt::index::Entry::new(
        b".gitmodules".to_vec(),
        girt::index::Mode::Symlink,
        id,
    )])
    .unwrap();
    edit.commit().unwrap();
    assert!(matches!(
        Modules::read(&repo, &objects, Source::Index),
        Err(LoadError::NotRegular)
    ));
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(
        Modules::read(&repo, &objects, Source::WorktreeOrIndex),
        Err(LoadError::NotRegular)
    ));
}

#[cfg(unix)]
#[test]
fn worktree_symlink_is_refused_without_index_fallback() {
    use girt::submodule::{LoadError, Source};
    let dir = fixture();
    std::fs::write(
        dir.path().join(".gitmodules"),
        b"[submodule \"n\"]\npath = vendor/lib\nurl = index\n",
    )
    .unwrap();
    git(dir.path(), &["add", ".gitmodules"], b"");
    std::fs::remove_file(dir.path().join(".gitmodules")).unwrap();
    std::fs::write(
        dir.path().join("linked"),
        b"[submodule \"n\"]\npath = vendor/lib\nurl = linked\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("linked", dir.path().join(".gitmodules")).unwrap();
    let repo = girt::Repository::open(dir.path()).unwrap();
    let objects = repo.objects().unwrap();
    assert!(matches!(
        Modules::read(&repo, &objects, Source::WorktreeOrIndex),
        Err(LoadError::NotRegular)
    ));
    // This is an intentional safety restriction beyond Git init's observed behavior.
    git(dir.path(), &["submodule", "init"], b"");
    assert_eq!(
        git(dir.path(), &["config", "--get", "submodule.n.url"], b""),
        b"linked\n"
    );
}

#[test]
fn relative_urls_match_git_init_for_standard_remote_shapes() {
    let dir = fixture();
    for remote in [
        "https://example.com/team/parent.git",
        "https://example.com/team/parent.git/",
        "user@host:team/parent.git",
        "file:///tmp/team/parent.git",
        "../parent.git",
        "./parent.git",
        "parent.git",
        "a//b",
        "/tmp/team/parent.git",
    ] {
        git(dir.path(), &["config", "remote.origin.url", remote], b"");
        for url in [
            "./child.git",
            "../child.git",
            "./foo/../child.git",
            "./../child.git",
        ] {
            let bytes = format!("[submodule \"logical\"]\npath = vendor/lib\nurl = {url}\n");
            std::fs::write(dir.path().join(".gitmodules"), &bytes).unwrap();
            let modules = Modules::parse(bytes.as_bytes()).unwrap();
            let resolved = modules
                .get(b"logical")
                .unwrap()
                .resolve_url(remote.as_bytes())
                .unwrap();
            git(dir.path(), &["submodule", "init"], b"");
            let actual = git(
                dir.path(),
                &["config", "--get", "submodule.logical.url"],
                b"",
            );
            assert_eq!(
                actual.strip_suffix(b"\n").unwrap(),
                resolved,
                "{remote} + {url}"
            );
            git(
                dir.path(),
                &["config", "--remove-section", "submodule.logical"],
                b"",
            );
        }
    }
}

#[test]
fn relative_url_endpoint_traversal_is_refused_and_absolute_urls_are_unchanged() {
    use girt::submodule::UrlError;
    let modules = Modules::parse(b"[submodule \"n\"]\nurl = ../../child\n").unwrap();
    for remote in [
        "https://example.com/parent.git",
        "user@host:parent.git",
        "/parent.git",
        "../parent.git",
    ] {
        assert_eq!(
            modules.get(b"n").unwrap().resolve_url(remote.as_bytes()),
            Err(UrlError::AboveRoot)
        );
    }
    let modules = Modules::parse(b"[submodule \"n\"]\nurl = https://example.com/child\n").unwrap();
    assert_eq!(
        modules.get(b"n").unwrap().resolve_url(b""),
        Ok(b"https://example.com/child".to_vec())
    );
}

#[test]
fn commit_source_is_available_in_a_bare_repository() {
    use girt::submodule::{LoadError, Source};
    let dir = fixture();
    std::fs::write(
        dir.path().join(".gitmodules"),
        b"[submodule \"n\"]\npath = vendor/lib\nurl = child\n",
    )
    .unwrap();
    git(dir.path(), &["add", ".gitmodules"], b"");
    git(dir.path(), &["commit", "-qm", "modules"], b"");
    git(
        dir.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--bare",
            "--quiet",
            ".",
            "bare",
        ],
        b"",
    );
    let repo = girt::Repository::open(dir.path().join("bare")).unwrap();
    let objects = repo.objects().unwrap();
    let head = git(dir.path(), &["rev-parse", "HEAD"], b"");
    let head = String::from_utf8_lossy(&head).trim().parse().unwrap();
    assert!(
        Modules::read(&repo, &objects, Source::Commit(head))
            .unwrap()
            .unwrap()
            .get(b"n")
            .is_some()
    );
    assert!(matches!(
        Modules::read(&repo, &objects, Source::Worktree),
        Err(LoadError::NoWorktree)
    ));
}

#[test]
fn repeated_remote_separators_match_git_without_crossing_root() {
    let dir = fixture();
    let bytes = b"[submodule \"logical\"]\npath = vendor/lib\nurl = ../../child\n";
    std::fs::write(dir.path().join(".gitmodules"), bytes).unwrap();
    git(dir.path(), &["config", "remote.origin.url", "a//b"], b"");
    git(dir.path(), &["submodule", "init"], b"");
    let modules = Modules::parse(bytes).unwrap();
    assert_eq!(
        modules
            .get(b"logical")
            .unwrap()
            .resolve_url(b"a//b")
            .unwrap(),
        b"a/child"
    );
    assert_eq!(
        git(
            dir.path(),
            &["config", "--get", "submodule.logical.url"],
            b""
        ),
        b"a/child\n"
    );
}

#[test]
fn intent_to_add_metadata_reads_the_empty_index_blob() {
    use girt::submodule::Source;
    let dir = fixture();
    let path = dir.path().join(".gitmodules");
    std::fs::write(
        &path,
        b"[submodule \"n\"]\npath = vendor/lib\nurl = child\n",
    )
    .unwrap();
    git(dir.path(), &["add", "--intent-to-add", ".gitmodules"], b"");
    assert!(git(dir.path(), &["show", ":.gitmodules"], b"").is_empty());
    std::fs::remove_file(path).unwrap();
    let repo = girt::Repository::open(dir.path()).unwrap();
    let objects = repo.objects().unwrap();
    assert!(
        Modules::read(&repo, &objects, Source::WorktreeOrIndex)
            .unwrap()
            .unwrap()
            .definitions()
            .is_empty()
    );
}
