//! Native `credential.helper = store` against `git credential-store`; Git required. The
//! authenticated HTTP fetch through the store is in `http.rs`.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::Config;
use girt::remote::{
    CredentialBackend, CredentialContext, CredentialHelper, CredentialSession, CredentialStore,
};

/// Characters that need percent-encoding in every credential field.
const USERNAME: &str = "u s@r:/%";
const PASSWORD: &str = "p:w/d%+#?é";

fn context(host: &str) -> CredentialContext {
    CredentialContext {
        protocol: b"https".to_vec(),
        host: host.as_bytes().to_vec(),
        path: Some(b"group/repo".to_vec()),
        use_http_path: false,
    }
}

/// Runs `git credential-store --file <file> <action>` with a credential description.
fn git_store(directory: &Path, file: &Path, action: &str, input: &str) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .arg("credential-store")
        .arg("--file")
        .arg(file)
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn session(file: &Path, context: CredentialContext) -> CredentialSession {
    let config = Config::parse(b"[credential]\nhelper = store\n").unwrap();
    let helper = CredentialHelper {
        configured_name: b"store".to_vec(),
        backend: CredentialBackend::Store(CredentialStore::file(file)),
    };
    let cancel = AtomicBool::new(false);
    CredentialSession::fill(&config, context, &[helper], None, &cancel, None).unwrap()
}

fn prompted(file: &Path, context: CredentialContext) -> CredentialSession {
    let config = Config::parse(b"[credential]\nhelper = store\n").unwrap();
    let helper = CredentialHelper {
        configured_name: b"store".to_vec(),
        backend: CredentialBackend::Store(CredentialStore::file(file)),
    };
    let cancel = AtomicBool::new(false);
    let mut prompt = |field| {
        Some(match field {
            girt::remote::Prompt::Username => USERNAME.as_bytes().to_vec(),
            girt::remote::Prompt::Password => PASSWORD.as_bytes().to_vec(),
        })
    };
    CredentialSession::fill(
        &config,
        context,
        &[helper],
        Some(&mut prompt),
        &cancel,
        None,
    )
    .unwrap()
}

#[test]
fn git_reads_what_girt_stores_and_both_skip_damaged_lines() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("credentials");
    std::fs::write(&file, "https://damaged@example.invalid\nnot a url\n").unwrap();
    let cancel = AtomicBool::new(false);
    prompted(&file, context("example.invalid:8443"))
        .approve(&cancel, None)
        .unwrap();

    let answer = git_store(
        directory.path(),
        &file,
        "get",
        "protocol=https\nhost=example.invalid:8443\n\n",
    );
    assert_eq!(
        answer,
        format!("username={USERNAME}\npassword={PASSWORD}\n")
    );
    // Neither reader treats the damaged lines as credentials; girt keeps them on rewrite.
    let contents = std::fs::read_to_string(&file).unwrap();
    assert!(contents.ends_with("https://damaged@example.invalid\nnot a url\n"));
    assert_eq!(
        git_store(
            directory.path(),
            &file,
            "get",
            "protocol=https\nhost=example.invalid\n\n"
        ),
        ""
    );
    assert!(
        !session(&file, context("example.invalid"))
            .credential()
            .complete()
    );
}

#[test]
fn girt_reads_what_git_stores() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("credentials");
    git_store(
        directory.path(),
        &file,
        "store",
        &format!(
            "protocol=https\nhost=example.invalid:8443\nusername={USERNAME}\npassword={PASSWORD}\n\n"
        ),
    );
    let mut contents = std::fs::read(&file).unwrap();
    contents.extend_from_slice(b"https://damaged@example.invalid:8443\n");
    std::fs::write(&file, contents).unwrap();

    let found = session(&file, context("example.invalid:8443"));
    assert_eq!(
        found.credential().username.as_deref(),
        Some(USERNAME.as_bytes())
    );
    assert_eq!(
        found.credential().password.as_deref(),
        Some(PASSWORD.as_bytes())
    );
}

#[test]
fn girt_writes_the_same_bytes_as_git() {
    let directory = tempfile::tempdir().unwrap();
    let by_git = directory.path().join("git");
    let by_girt = directory.path().join("girt");
    let seed = "https://kept:1@other.invalid\ndamaged line\n";
    std::fs::write(&by_git, seed).unwrap();
    std::fs::write(&by_girt, seed).unwrap();
    let request = format!(
        "protocol=https\nhost=example.invalid:8443\npath=group/repo\nusername={USERNAME}\npassword={PASSWORD}\n\n"
    );
    git_store(directory.path(), &by_git, "store", &request);
    let cancel = AtomicBool::new(false);
    let mut path_context = context("example.invalid:8443");
    path_context.use_http_path = true;
    prompted(&by_girt, path_context.clone())
        .approve(&cancel, None)
        .unwrap();
    assert_eq!(
        std::fs::read(&by_girt).unwrap(),
        std::fs::read(&by_git).unwrap()
    );

    git_store(directory.path(), &by_git, "erase", &request);
    session(&by_girt, path_context)
        .reject(&cancel, None)
        .unwrap();
    assert_eq!(
        std::fs::read(&by_girt).unwrap(),
        std::fs::read(&by_git).unwrap()
    );
    assert_eq!(std::fs::read_to_string(&by_girt).unwrap(), seed);
}

#[cfg(unix)]
#[test]
fn created_file_is_owner_only_like_git() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("credentials");
    let cancel = AtomicBool::new(false);
    prompted(&file, context("example.invalid"))
        .approve(&cancel, None)
        .unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
