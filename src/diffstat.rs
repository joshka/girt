//! Bounded per-file change counts for diff summaries.
//!
//! Callers supply counts after applying their diff attributes and content-diff policy. A
//! [`Diffstat`] preserves that order and renders an owned artifact before the caller writes it.
//! [`Diffstat::numstat`] uses Git's decimal counts and path quoting; its NUL form preserves raw
//! paths, including both sides of renames and copies. This module does not inspect repositories.
//!
//! ```
//! use girt::diffstat::{Counts, Diffstat, FileStat, Limits};
//!
//! let stat = Diffstat::new(
//!     vec![FileStat {
//!         path: b"src/main.rs".to_vec(),
//!         source_path: None,
//!         counts: Counts::Text {
//!             added: 3,
//!             removed: 1,
//!         },
//!     }],
//!     Limits::default(),
//! )?;
//! assert_eq!(stat.numstat(false)?, b"3\t1\tsrc/main.rs\n");
//! # Ok::<(), girt::diffstat::Error>(())
//! ```

/// Validated per-file counts, in caller-selected order.
#[derive(Clone, Debug)]
pub struct Diffstat {
    files: Vec<FileStat>,
    limits: Limits,
}

impl Diffstat {
    /// Takes counts without copying paths, checking file count and aggregate path bytes.
    ///
    /// # Errors
    ///
    /// Refuses exceeded limits, empty paths, or paths containing NUL. Paths need not be UTF-8;
    /// this validates their output representation, not their suitability for filesystem access.
    pub fn new(files: Vec<FileStat>, limits: Limits) -> Result<Self, Error> {
        if files.len() > limits.max_files {
            return Err(Error::Limit("files"));
        }
        let mut bytes = 0usize;
        for file in &files {
            for path in std::iter::once(&file.path).chain(file.source_path.iter()) {
                if path.is_empty() || path.contains(&0) {
                    return Err(Error::InvalidPath);
                }
                bytes = bytes
                    .checked_add(path.len())
                    .ok_or(Error::Limit("path bytes"))?;
                if bytes > limits.max_path_bytes {
                    return Err(Error::Limit("path bytes"));
                }
            }
        }
        Ok(Self { files, limits })
    }

    /// Borrows the validated entries in their original order.
    #[must_use]
    pub fn files(&self) -> &[FileStat] {
        &self.files
    }

    /// Renders Git's `--numstat` format, optionally with NUL record terminators (`-z`).
    ///
    /// Text records use decimal additions and deletions; binary records use `-` for both.
    /// With NUL termination a rename or copy has an empty path field followed by the original
    /// and destination paths, each NUL terminated. Otherwise shared directory components are
    /// compacted and paths use [`crate::display::quote_path`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] before returning any artifact when the output exceeds the byte
    /// budget. Input and output allocations are bounded independently; allocation failure follows
    /// Rust's allocator behavior.
    pub fn numstat(&self, nul: bool) -> Result<Vec<u8>, Error> {
        let mut out = Output {
            bytes: Vec::new(),
            limit: self.limits.max_output_bytes,
        };
        for file in &self.files {
            match file.counts {
                Counts::Text { added, removed } => {
                    out.append(added.to_string().as_bytes())?;
                    out.append(b"\t")?;
                    out.append(removed.to_string().as_bytes())?;
                }
                Counts::Binary { .. } => out.append(b"-\t-")?,
            }
            out.append(b"\t")?;
            if nul {
                if let Some(source) = &file.source_path {
                    out.append(b"\0")?;
                    out.append(source)?;
                    out.append(b"\0")?;
                }
                out.append(&file.path)?;
                out.append(b"\0")?;
            } else {
                let path = text_path(file);
                out.append(path.as_bytes())?;
                out.append(b"\n")?;
            }
        }
        Ok(out.bytes)
    }
}

/// Counts for one destination path, optionally paired with its rename or copy source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileStat {
    /// Destination path, or original path for a deletion; arbitrary nonempty bytes without NUL.
    pub path: Vec<u8>,
    /// Original path for a rename or copy; the renderer does not distinguish these operations.
    pub source_path: Option<Vec<u8>>,
    /// Content changes after the caller's attributes and text-conversion policy.
    pub counts: Counts,
}

/// Text line changes or binary byte lengths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Counts {
    /// Inserted and deleted lines, including a final line without a newline.
    Text {
        /// Inserted lines.
        added: u64,
        /// Deleted lines.
        removed: u64,
    },
    /// Binary contents; numstat renders dashes rather than these sizes.
    Binary {
        /// Original content length in bytes; zero for an addition.
        old_bytes: u64,
        /// Resulting content length in bytes; zero for a deletion.
        new_bytes: u64,
    },
}

/// Independent bounds on retained input and rendered artifacts.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum file entries, including entries with zero changed lines; default 100,000.
    pub max_files: usize,
    /// Aggregate bytes in both destination and source paths; default 16 MiB.
    pub max_path_bytes: usize,
    /// Maximum bytes in each returned artifact; default 64 MiB.
    pub max_output_bytes: usize,
}

impl Limits {
    /// Sets [`Self::max_files`].
    #[must_use]
    pub fn with_max_files(mut self, max_files: usize) -> Self {
        self.max_files = max_files;
        self
    }

    /// Sets [`Self::max_path_bytes`].
    #[must_use]
    pub fn with_max_path_bytes(mut self, max_path_bytes: usize) -> Self {
        self.max_path_bytes = max_path_bytes;
        self
    }

    /// Sets [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_path_bytes: 16 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Invalid summary input or a caller-selected resource bound.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A named resource would exceed its configured bound.
    #[error("diffstat exceeds {0} limit")]
    Limit(&'static str),
    /// A path is empty or contains NUL, which cannot represent a Git path.
    #[error("diffstat paths must be nonempty and contain no NUL")]
    InvalidPath,
}

struct Output {
    bytes: Vec<u8>,
    limit: usize,
}

impl Output {
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Error::Limit("output bytes"))?;
        if len > self.limit {
            return Err(Error::Limit("output bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

// Git keeps separately quoted rename sides instead of compacting an escaped path. Quoting
// expands each byte at most fourfold, so the path budget also bounds these temporary strings.
fn text_path(file: &FileStat) -> String {
    let new = crate::display::quote_path(&file.path).to_string();
    let Some(source) = &file.source_path else {
        return new;
    };
    let old = crate::display::quote_path(source).to_string();
    if old.starts_with('"') || new.starts_with('"') {
        return format!("{old} => {new}");
    }
    // Both paths are printable ASCII after the escaping check above.
    String::from_utf8(compact_path(source, &file.path)).expect("printable ASCII paths")
}

// Only complete directory components are shared. Keep the changed middle visible even when the
// two names are identical: callers use source_path to request rename/copy record framing.
fn compact_path(old: &[u8], new: &[u8]) -> Vec<u8> {
    let common = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let prefix = old[..common]
        .iter()
        .rposition(|&b| b == b'/')
        .map_or(0, |i| i + 1);
    let shared_len = old.len().min(new.len());
    // The separating slash may serve as both the end of the prefix and the start of the
    // suffix. This is what lets `a/x/b` and `a/b` render as `a/{x => }/b`.
    let overlaps_separator = prefix != 0 && old[prefix - 1] == b'/';
    let suffix_limit = shared_len - prefix + usize::from(overlaps_separator);
    let suffix_common = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(suffix_limit)
        .take_while(|(a, b)| a == b)
        .count();
    let shared_suffix = &old[old.len() - suffix_common..];
    let slash = shared_suffix.iter().position(|&byte| byte == b'/');
    let suffix = slash.map_or(0, |index| suffix_common - index);
    let mut path = Vec::new();
    path.extend_from_slice(&old[..prefix]);
    let braces = prefix != 0 || suffix != 0;
    if braces {
        path.push(b'{');
    }
    path.extend_from_slice(&old[prefix..(old.len() - suffix).max(prefix)]);
    path.extend_from_slice(b" => ");
    path.extend_from_slice(&new[prefix..(new.len() - suffix).max(prefix)]);
    if braces {
        path.push(b'}');
    }
    path.extend_from_slice(&old[old.len() - suffix..]);
    path
}
