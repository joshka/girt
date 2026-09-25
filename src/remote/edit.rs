use super::{Direction, Refspec, RefspecError};
use crate::config::{ConfigError, Document};

/// Configuration-only remote operations on one explicit direct-file document.
///
/// Includes and inherited files are never edited or evaluated. Success describes local syntax,
/// not disappearance of an effective remote: included/global values can remain. Re-resolve with
/// the original inputs after publication. These operations neither rename/delete remote-tracking
/// references nor update a consumer's view. Names are subsection bytes, not jj selection policy.
/// URLs are opaque configuration bytes; endpoint/protocol validation belongs to transport.
///
/// Each operation prepares a clone and replaces the document only on success. Branch references
/// and `remote.pushDefault` are changed only in this document. Ambiguous repeated scalar branch
/// selectors are rejected rather than partially applying Git CLI's warning-producing behavior.
pub struct RemoteConfig<'a> {
    document: &'a mut Document,
}

/// A local remote configuration operation could not be prepared; no draft changes were applied.
#[derive(Debug, thiserror::Error)]
pub enum RemoteEditError {
    /// No local section exists; an inherited remote may still be effective.
    #[error("remote has no section in the selected file")]
    NotLocal,
    /// Destination section already exists in the selected file (even if empty).
    #[error("remote section already exists in the selected file")]
    Exists,
    /// Repeated branch/remote scalar selectors would make a coherent update ambiguous.
    #[error("repeated scalar remote selector requires explicit occurrence editing")]
    AmbiguousSelector,
    /// The selected URL/refspec occurrence does not exist.
    #[error("remote value occurrence does not exist")]
    MissingOccurrence,
    /// Configuration syntax cannot represent the supplied bytes.
    #[error(transparent)]
    Syntax(#[from] ConfigError),
    /// Refspec is invalid or outside the supported operational subset.
    #[error(transparent)]
    Refspec(#[from] RefspecError),
}

/// One of the four ordered remote configuration lists.
#[derive(Debug, Clone, Copy)]
pub enum RemoteKey {
    /// Fetch URL list; empty values reset prior URLs.
    Url,
    /// Explicit push URL list; an empty resulting list falls back to URL values.
    PushUrl,
    /// Fetch refspecs, validated at this operation boundary.
    Fetch,
    /// Push refspecs, validated at this operation boundary.
    Push,
}
impl RemoteKey {
    fn name(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::PushUrl => "pushurl",
            Self::Fetch => "fetch",
            Self::Push => "push",
        }
    }
    fn validate(self, value: &[u8]) -> Result<(), RemoteEditError> {
        match self {
            Self::Fetch => {
                Refspec::parse(Direction::Fetch, value)?;
            }
            Self::Push => {
                Refspec::parse(Direction::Push, value)?;
            }
            Self::Url | Self::PushUrl => {}
        }
        Ok(())
    }
}
impl<'a> RemoteConfig<'a> {
    /// Selects a direct-file draft; no effective snapshot is accepted or cached.
    pub fn new(document: &'a mut Document) -> Self {
        Self { document }
    }

    /// Adds a local remote with an explicit URL and ordered fetch mappings.
    ///
    /// No default mapping is guessed from the name. Existing inherited values can precede or
    /// follow these occurrences when resolved. An empty local header counts as already present.
    ///
    /// # Errors
    ///
    /// Existing local sections, unrepresentable bytes or invalid refspecs leave the draft
    /// unchanged.
    pub fn add(&mut self, name: &[u8], url: &[u8], fetch: &[&[u8]]) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            if draft.contains_section("remote", Some(name)) {
                return Err(RemoteEditError::Exists);
            }
            draft.append("remote", Some(name), "url", url)?;
            for value in fetch {
                RemoteKey::Fetch.validate(value)?;
                draft.append("remote", Some(name), "fetch", value)?;
            }
            Ok(())
        })
    }

    /// Appends one URL or refspec, preserving duplicates and existing order.
    ///
    /// # Errors
    ///
    /// Requires a local section and representable bytes; refspec keys also require valid mappings.
    pub fn append(
        &mut self,
        name: &[u8],
        key: RemoteKey,
        value: &[u8],
    ) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            require_local(draft, name)?;
            key.validate(value)?;
            draft.append("remote", Some(name), key.name(), value)?;
            Ok(())
        })
    }

    /// Replaces a zero-based local occurrence of a URL or refspec.
    ///
    /// Selection is explicit and byte-oriented, including repeated URLs. Git CLI can refuse
    /// an unqualified set-url when multiple values exist; regex selection is not provided here.
    ///
    /// # Errors
    ///
    /// Missing occurrences or invalid replacement values leave the draft unchanged.
    pub fn set(
        &mut self,
        name: &[u8],
        key: RemoteKey,
        occurrence: usize,
        value: &[u8],
    ) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            key.validate(value)?;
            let index = occurrence_index(draft, name, key, occurrence)?;
            draft.set_value(index, value)?;
            Ok(())
        })
    }

    /// Removes one local list occurrence; removing push URLs can enable URL fallback.
    ///
    /// # Errors
    ///
    /// A missing occurrence leaves the draft unchanged. No inherited value is removed.
    pub fn remove_value(
        &mut self,
        name: &[u8],
        key: RemoteKey,
        occurrence: usize,
    ) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            let index = occurrence_index(draft, name, key, occurrence)?;
            draft.remove(index)?;
            Ok(())
        })
    }

    /// Renames local sections and local branch remote/pushRemote and remote.pushDefault selectors.
    ///
    /// Fetch destinations under `refs/remotes/<old>/` are renamed; custom destinations and push
    /// refspecs remain exact. Remote-tracking refs themselves are not changed. Repeated sections
    /// and list order are preserved. Identical names are a no-op after checking local existence.
    ///
    /// # Errors
    ///
    /// Missing source, existing destination, ambiguous scalar selectors or invalid bytes preserve
    /// the entire draft. Inherited references remain unchanged and require explicit caller action.
    pub fn rename(&mut self, old: &[u8], new: &[u8]) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            require_local(draft, old)?;
            if old == new {
                return Ok(());
            }
            if draft.contains_section("remote", Some(new)) {
                return Err(RemoteEditError::Exists);
            }
            update_selectors(draft, old, Some(new))?;
            let old_prefix = remote_prefix(old);
            let new_prefix = remote_prefix(new);
            let replacements: Vec<_> = draft
                .config()
                .entries()
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.section.eq_ignore_ascii_case(b"remote")
                        && e.subsection.as_deref() == Some(old)
                        && e.name.eq_ignore_ascii_case(b"fetch")
                })
                .filter_map(|(index, e)| {
                    let value = e.value.as_deref()?;
                    let colon = value.iter().position(|b| *b == b':')?;
                    let suffix = value[colon + 1..].strip_prefix(old_prefix.as_slice())?;
                    let mut replacement = value[..colon + 1].to_vec();
                    replacement.extend_from_slice(&new_prefix);
                    replacement.extend_from_slice(suffix);
                    Some((index, replacement))
                })
                .collect();
            for (index, value) in replacements {
                RemoteKey::Fetch.validate(&value)?;
                draft.set_value(index, &value)?;
            }
            draft.rename_section("remote", Some(old), Some(new))?;
            Ok(())
        })
    }

    /// Removes local remote syntax and its local branch selectors.
    ///
    /// A branch whose single `remote` equals this name loses `remote` and all `merge` entries;
    /// matching `pushRemote` and `remote.pushDefault` are removed independently. Other branch
    /// settings survive. Success does not claim that an inherited remote ceased to exist.
    ///
    /// # Errors
    ///
    /// Inherited-only/missing remotes and repeated scalar selectors fail without draft changes.
    pub fn remove(&mut self, name: &[u8]) -> Result<(), RemoteEditError> {
        self.prepare(|draft| {
            require_local(draft, name)?;
            update_selectors(draft, name, None)?;
            draft.remove_section("remote", Some(name))?;
            Ok(())
        })
    }

    fn prepare(
        &mut self,
        edit: impl FnOnce(&mut Document) -> Result<(), RemoteEditError>,
    ) -> Result<(), RemoteEditError> {
        let mut draft = self.document.clone();
        edit(&mut draft)?;
        *self.document = draft;
        Ok(())
    }
}
fn require_local(document: &Document, name: &[u8]) -> Result<(), RemoteEditError> {
    if document.contains_section("remote", Some(name)) {
        Ok(())
    } else {
        Err(RemoteEditError::NotLocal)
    }
}
fn occurrence_index(
    document: &Document,
    name: &[u8],
    key: RemoteKey,
    occurrence: usize,
) -> Result<usize, RemoteEditError> {
    require_local(document, name)?;
    document
        .config()
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.section.eq_ignore_ascii_case(b"remote")
                && e.subsection.as_deref() == Some(name)
                && e.name.eq_ignore_ascii_case(key.name().as_bytes())
        })
        .nth(occurrence)
        .map(|(index, _)| index)
        .ok_or(RemoteEditError::MissingOccurrence)
}
fn remote_prefix(name: &[u8]) -> Vec<u8> {
    let mut prefix = b"refs/remotes/".to_vec();
    prefix.extend_from_slice(name);
    prefix.push(b'/');
    prefix
}
fn update_selectors(
    document: &mut Document,
    old: &[u8],
    new: Option<&[u8]>,
) -> Result<(), RemoteEditError> {
    let entries = document.config().entries();
    let mut selectors = Vec::new();
    let mut branches = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let branch = entry.section.eq_ignore_ascii_case(b"branch") && entry.subsection.is_some();
        let selector = (branch
            && (entry.name.eq_ignore_ascii_case(b"remote")
                || entry.name.eq_ignore_ascii_case(b"pushremote")))
            || (entry.section.eq_ignore_ascii_case(b"remote")
                && entry.subsection.is_none()
                && entry.name.eq_ignore_ascii_case(b"pushdefault"));
        if !selector || entry.value.as_deref() != Some(old) {
            continue;
        }
        let count = entries
            .iter()
            .filter(|e| {
                e.section.eq_ignore_ascii_case(&entry.section)
                    && e.subsection == entry.subsection
                    && e.name.eq_ignore_ascii_case(&entry.name)
            })
            .count();
        if count != 1 {
            return Err(RemoteEditError::AmbiguousSelector);
        }
        selectors.push(index);
        if branch && entry.name.eq_ignore_ascii_case(b"remote") {
            branches.push(entry.subsection.clone());
        }
    }
    if let Some(new) = new {
        for index in selectors {
            document.set_value(index, new)?;
        }
    } else {
        selectors.extend(
            entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.section.eq_ignore_ascii_case(b"branch")
                        && branches.contains(&e.subsection)
                        && e.name.eq_ignore_ascii_case(b"merge")
                })
                .map(|(index, _)| index),
        );
        selectors.sort_unstable();
        for index in selectors.into_iter().rev() {
            document.remove(index)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::inherited_only(b"[include]\npath=other\n", b"old", b"new")]
    #[case::destination(b"[remote \"old\"]\nurl=x\n[remote \"new\"]\n", b"old", b"new")]
    #[case::ambiguous(
        b"[remote \"old\"]\nurl=x\n[branch \"b\"]\nremote=old\nremote=other\n",
        b"old",
        b"new"
    )]
    #[case::invalid_name(b"[remote \"old\"]\nurl=x\n", b"old", b"n\n")]
    fn failed_rename_is_transactional(
        #[case] bytes: &[u8],
        #[case] old: &[u8],
        #[case] new: &[u8],
    ) {
        let mut document = Document::parse(bytes).unwrap();
        assert!(RemoteConfig::new(&mut document).rename(old, new).is_err());
        assert_eq!(document.as_bytes(), bytes);
    }
    #[test]
    fn invalid_second_mapping_does_not_add_partial_remote() {
        let mut document = Document::parse(b"#keep\n").unwrap();
        assert!(
            RemoteConfig::new(&mut document)
                .add(
                    b"origin",
                    b"opaque",
                    &[b"refs/heads/*:refs/remotes/o/*", b"bad space"]
                )
                .is_err()
        );
        assert_eq!(document.as_bytes(), b"#keep\n");
    }
    #[test]
    fn repeated_list_occurrences_and_push_fallback() {
        let mut document =
            Document::parse(b"[remote \"o\"]\nurl=first\nurl=second\npushurl=push\n").unwrap();
        let mut edit = RemoteConfig::new(&mut document);
        edit.set(b"o", RemoteKey::Url, 0, b"new").unwrap();
        edit.append(b"o", RemoteKey::Url, b"second").unwrap();
        edit.remove_value(b"o", RemoteKey::PushUrl, 0).unwrap();
        let remote = super::super::Remote::find(document.config(), b"o")
            .unwrap()
            .unwrap();
        assert_eq!(
            remote.urls(),
            &[b"new".to_vec(), b"second".to_vec(), b"second".to_vec()]
        );
        assert_eq!(remote.push_urls(), remote.urls());
    }
    #[test]
    fn removal_preserves_unrelated_branch_settings_and_comments() {
        let mut document = Document::parse(b"[remote \"o\"]\nurl=x #url\n[branch \"b\"]\nremote=o\nmerge=refs/heads/a\nmerge=refs/heads/b\npushRemote=o\nrebase=true #keep\n[remote]\npushDefault=o\n").unwrap();
        RemoteConfig::new(&mut document).remove(b"o").unwrap();
        assert_eq!(document.config().entries().len(), 1);
        assert_eq!(
            document.config().value("branch", Some(b"b"), "rebase"),
            Some(Some(b"true".as_slice()))
        );
        assert!(document.as_bytes().windows(5).any(|b| b == b"#keep"));
        assert!(document.as_bytes().windows(4).any(|b| b == b"#url"));
    }
}
