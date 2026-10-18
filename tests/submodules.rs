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
