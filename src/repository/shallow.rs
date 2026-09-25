//! Immutable, format-aware shallow boundary metadata.
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{ObjectFormat, ObjectId};

/// A fixed set of declared history boundaries. Object existence and kind are checked when walked.
///
/// Duplicate lines are coalesced and IDs are sorted. Empty or absent files mean no boundaries.
/// Reading uses one file descriptor: atomic Git replacement yields the old or new file, never a
/// mixture. In-place external edits are not synchronized; callers must exclude those writers.
#[derive(Clone, Debug)]
pub struct ShallowRoots {
    format: ObjectFormat,
    roots: BTreeSet<ObjectId>,
}

/// Failure reading shallow metadata; no repository files are changed.
#[derive(Debug, thiserror::Error)]
pub enum ShallowError {
    /// Filesystem failure, including inaccessible metadata.
    #[error("cannot read shallow metadata {path}: {source}")]
    Io {
        /// File being read.
        path: PathBuf,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A line is not a full hexadecimal ID in the repository format.
    #[error("invalid shallow root on line {line}")]
    InvalidRoot {
        /// One-based line number. Wrong-width IDs are invalid as well.
        line: usize,
    },
    /// The supplied byte budget is exhausted.
    #[error("shallow metadata byte limit exceeded")]
    Limit,
    /// Cancellation was observed before opening or between bounded reads/lines.
    #[error("shallow metadata read cancelled")]
    Cancelled,
}

impl ShallowRoots {
    pub(crate) fn empty(format: ObjectFormat) -> Self {
        Self {
            format,
            roots: BTreeSet::new(),
        }
    }

    /// Reads `path` with a byte budget and cooperative cancellation. Missing files are empty.
    ///
    /// LF and CRLF records and a final record without a newline are accepted. Blank records,
    /// whitespace, non-hexadecimal bytes and wrong-format IDs are rejected. The default
    /// repository opener allows 16 MiB. Missing/non-commit objects are retained as declarations;
    /// traversal still requires every visited boundary to be a readable, parseable commit. Null
    /// IDs are retained as declarations, like other identities whose objects may not exist.
    ///
    /// # Errors
    ///
    /// Returns structured I/O, line, budget or cancellation failures. A filesystem read cannot
    /// be interrupted. No object storage is accessed and no files are written.
    pub fn read(
        path: impl AsRef<Path>,
        format: ObjectFormat,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<Self, ShallowError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "repository.shallow", outcome = "incomplete", failure_class = tracing::field::Empty, effects = tracing::field::Empty);
        let operation = || {
            let path = path.as_ref();
            check(cancel)?;
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Self::empty(format));
                }
                Err(source) => {
                    return Err(ShallowError::Io {
                        path: path.into(),
                        source,
                    });
                }
            };
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                check(cancel)?;
                let count = file.read(&mut chunk).map_err(|source| ShallowError::Io {
                    path: path.into(),
                    source,
                })?;
                if count == 0 {
                    break;
                }
                if count > max_bytes.saturating_sub(bytes.len()) {
                    return Err(ShallowError::Limit);
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Self::parse(&bytes, format, cancel)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| match error {
            ShallowError::Io { .. } => "io",
            ShallowError::InvalidRoot { .. } => "corrupt",
            ShallowError::Limit => "limit",
            ShallowError::Cancelled => "cancelled",
        });
        result
    }

    fn parse(
        bytes: &[u8],
        format: ObjectFormat,
        cancel: &AtomicBool,
    ) -> Result<Self, ShallowError> {
        let mut result = Self::empty(format);
        if bytes.is_empty() {
            return Ok(result);
        }
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            check(cancel)?;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let id = std::str::from_utf8(line)
                .ok()
                .and_then(|line| ObjectId::from_hex(format, line).ok())
                .ok_or(ShallowError::InvalidRoot { line: index + 1 })?;
            result.roots.insert(id);
        }
        Ok(result)
    }

    /// Format used to parse every root, including an empty snapshot.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }
    /// Sorted, unique declared roots.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = ObjectId> + '_ {
        self.roots.iter().copied()
    }
    /// Whether no boundaries are declared.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
    /// Whether this identity is a declared boundary. Wrong-format IDs never match.
    pub fn contains(&self, id: ObjectId) -> bool {
        self.roots.contains(&id)
    }
}

fn check(cancel: &AtomicBool) -> Result<(), ShallowError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ShallowError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn duplicate_roots_are_coalesced(#[case] format: ObjectFormat) {
        let id = ObjectId::for_blob(format, b"declaration");
        let bytes = format!("{id}\r\n{id}");
        let roots = ShallowRoots::parse(bytes.as_bytes(), format, &AtomicBool::new(false)).unwrap();
        assert_eq!(roots.iter().collect::<Vec<_>>(), vec![id]);
    }

    #[rstest]
    #[case::blank(b"\n")]
    #[case::invalid(b"xyz\n")]
    #[case::wrong_format(b"1111111111111111111111111111111111111111111111111111111111111111\n")]
    fn invalid_root_has_line(#[case] bytes: &[u8]) {
        assert!(matches!(
            ShallowRoots::parse(bytes, ObjectFormat::Sha1, &AtomicBool::new(false)),
            Err(ShallowError::InvalidRoot { line: 1 })
        ));
    }

    #[test]
    fn read_bounds_and_cancellation_precede_results() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shallow");
        std::fs::write(&path, b"bad").unwrap();
        assert!(matches!(
            ShallowRoots::read(&path, ObjectFormat::Sha1, 0, &AtomicBool::new(false)),
            Err(ShallowError::Limit)
        ));
        assert!(matches!(
            ShallowRoots::read(&path, ObjectFormat::Sha1, 0, &AtomicBool::new(true)),
            Err(ShallowError::Cancelled)
        ));
        assert_eq!(std::fs::read(path).unwrap(), b"bad");
    }
}
