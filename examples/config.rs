//! Resolve inherited settings and inspect where a remote URL came from.
use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
use girt::{Config, InitKind, ObjectFormat, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("repo");
    Repository::init(ObjectFormat::Sha1, &path, InitKind::Worktree)?;

    let global = temporary.path().join("global");
    std::fs::write(
        &global,
        b"[remote \"origin\"]\nurl=https://example.com/project\n",
    )?;

    let inputs = ConfigInputs::default()
        .with_files(vec![
            ConfigFile::new(global, ConfigScope::Global).with_optional(false),
        ])
        .with_command(Some(Config::parse(b"[demo]\nmode=explicit\n")?));
    let repository = Repository::open_with_config(&path, &inputs)?;
    let remote = girt::remote::Remote::find(repository.config(), b"origin")?.unwrap();
    assert_eq!(
        remote.fetch_url(),
        Some(b"https://example.com/project".as_slice())
    );

    let origin = repository.config().entries()[0].origin.as_ref().unwrap();
    assert_eq!(origin.scope, ConfigScope::Global);
    assert_eq!(origin.location.line, 2);

    // Diagnostic display is caller policy; the library never traces paths or raw values.
    println!(
        "Remote URL came from {:?}, line {}",
        origin.scope, origin.location.line
    );

    temporary.close()?;
    Ok(())
}
