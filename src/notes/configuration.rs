use super::Error;
use crate::Config;
use crate::refs::RefName;
use crate::workflow::Environment;

/// Explicit notes reference selection, without reading process environment or publishing refs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Selection {
    /// Ref to read or update: environment, then `core.notesRef`, then `refs/notes/commits`.
    pub primary: RefName,
    /// Additional `notes.displayRef` patterns, in configuration order, without glob expansion.
    pub display_patterns: Vec<Vec<u8>>,
}
impl Selection {
    /// Resolves the primary ref and retains additional display patterns from explicit inputs.
    ///
    /// Primary names must be full names under `refs/notes/`, as required by Git for writes. Display
    /// patterns remain caller policy: expand them against a reference snapshot and include the
    /// primary ref. This does not implement `GIT_NOTES_DISPLAY_REF`.
    ///
    /// # Errors
    ///
    /// Reports an invalid or non-notes primary reference name.
    pub fn from_config(config: &Config, environment: &Environment) -> Result<Self, Error> {
        let primary = environment
            .get("GIT_NOTES_REF")
            .map(|v| v.as_encoded_bytes())
            .or_else(|| config.string("core", None, "notesRef"))
            .unwrap_or(b"refs/notes/commits");
        let bytes = primary.to_vec();
        if !bytes.starts_with(b"refs/notes/") {
            return Err(Error::Reference {
                name: bytes,
                source: None,
            });
        }
        let primary = RefName::new(&bytes).map_err(|source| Error::Reference {
            name: bytes,
            source: Some(source),
        })?;
        let display_patterns = config
            .values("notes", None, "displayRef")
            .flatten()
            .map(<[u8]>::to_vec)
            .collect();
        Ok(Self {
            primary,
            display_patterns,
        })
    }
}
