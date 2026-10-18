//! Configuration that makes a remote a partial-clone promisor.

use super::ObjectFilter;
use crate::Repository;
use crate::config::{Document, EditError};

/// Upper bound on the local configuration file this edit reads.
const MAX_CONFIG_BYTES: usize = 16 << 20;

/// Records `remote` as a promisor fetched with `filter`, as `git clone --filter` does.
///
/// Sets `remote.<name>.promisor = true` and `remote.<name>.partialclonefilter = <spec>`, and raises
/// `core.repositoryformatversion` from 0 to 1. `extensions.partialClone` is not written; Git reads
/// the promisor remote from `remote.<name>.promisor`. Values that already match are left as they
/// are, so the file is not rewritten when nothing changes.
pub(crate) fn register_promisor(
    repository: &Repository,
    remote: &str,
    filter: &ObjectFilter,
) -> Result<(), EditError> {
    let spec = filter.to_string();
    let config = repository.config();
    let subsection = Some(remote.as_bytes());
    if config.boolean("remote", subsection, "promisor") == Ok(Some(true))
        && config.string("remote", subsection, "partialclonefilter") == Some(spec.as_bytes())
        && repository.format_version() != 0
    {
        return Ok(());
    }
    let raise_version = repository.format_version() == 0;
    let mut edit = repository.edit_config(MAX_CONFIG_BYTES)?;
    let result = (|| {
        let document = edit.document_mut();
        set(document, "remote", subsection, "promisor", b"true")?;
        set(
            document,
            "remote",
            subsection,
            "partialclonefilter",
            spec.as_bytes(),
        )?;
        if raise_version {
            set(document, "core", None, "repositoryformatversion", b"1")?;
        }
        Ok::<_, crate::ConfigError>(())
    })();
    match result {
        Ok(()) => edit.commit(),
        Err(source) => {
            let path = repository.common_dir().join("config");
            edit.abort()?;
            Err(EditError::Format { path, source })
        }
    }
}

/// Replaces the last assignment of `name`, which is the one readers use, or appends one.
fn set(
    document: &mut Document,
    section: &str,
    subsection: Option<&[u8]>,
    name: &str,
    value: &[u8],
) -> Result<(), crate::ConfigError> {
    let last = document.config().entries().iter().rposition(|entry| {
        entry.section.eq_ignore_ascii_case(section.as_bytes())
            && entry.subsection.as_deref() == subsection
            && entry.name.eq_ignore_ascii_case(name.as_bytes())
    });
    match last {
        Some(occurrence) => document.set_value(occurrence, value),
        None => document.append(section, subsection, name, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat};

    #[test]
    fn registers_promisor_and_raises_format_version() {
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(ObjectFormat::Sha1, root.path().join("repo"), InitKind::Bare).unwrap();
        register_promisor(&repo, "origin", &ObjectFilter::BlobLimit(1024)).unwrap();
        let repo = Repository::open(repo.git_dir()).unwrap();
        let config = repo.config();
        assert_eq!(
            config
                .boolean("remote", Some(b"origin"), "promisor")
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            config.string("remote", Some(b"origin"), "partialclonefilter"),
            Some(&b"blob:limit=1024"[..])
        );
        assert_eq!(repo.format_version(), 1);
        assert!(config.string("extensions", None, "partialclone").is_none());

        let before = std::fs::read(repo.git_dir().join("config")).unwrap();
        register_promisor(&repo, "origin", &ObjectFilter::BlobLimit(1024)).unwrap();
        assert_eq!(
            std::fs::read(repo.git_dir().join("config")).unwrap(),
            before
        );
    }
}
