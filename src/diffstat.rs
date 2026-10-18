//! Bounded per-file change counts for diff summaries.
//!
//! Callers supply counts after applying their diff attributes and content-diff policy. A
//! [`Diffstat`] preserves that order and renders an owned artifact before the caller writes it.
//! [`Diffstat::numstat`] uses Git's decimal counts and path quoting; its NUL form preserves raw
//! paths, including both sides of renames and copies. This module does not inspect repositories.
//!
//! ```
//! use girt::diffstat::{Counts, Diffstat, FileStat, Limits, StatLayout};
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
//! assert_eq!(
//!     stat.stat(StatLayout::default())?,
//!     b" src/main.rs | 4 +++-\n 1 file changed, 3 insertions(+), 1 deletion(-)\n"
//! );
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

    /// Renders Git's human-readable `--stat` rows and summary at an explicit width.
    ///
    /// The layout is deterministic and does not read `COLUMNS` or terminal state. Command-line
    /// callers resolve those inputs and pass the resulting width. Rename and copy paths use Git's
    /// compact path notation. Binary rows show old and new byte lengths.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] before returning any artifact when the output exceeds the byte
    /// budget. Widths are capped to prevent an accidental oversized padding allocation.
    pub fn stat(&self, layout: StatLayout) -> Result<Vec<u8>, Error> {
        if layout.width > self.limits.max_layout_width {
            return Err(Error::Limit("layout width"));
        }
        if self.files.is_empty() {
            return Ok(Vec::new());
        }
        let mut output = Output {
            bytes: Vec::new(),
            limit: self.limits.max_output_bytes,
        };
        let paths = self.files.iter().map(text_path).collect::<Vec<_>>();
        let longest = paths.iter().map(String::len).max().unwrap_or_default();
        let requested_name_width = layout.width.saturating_mul(2).div_ceil(3);
        let available_name_width = layout.width.saturating_sub(13).max(12);
        let minimum_name_width = longest.saturating_add(2).min(12);
        let name_width = longest
            .saturating_add(2)
            .min(requested_name_width)
            .min(available_name_width)
            .max(minimum_name_width);
        let name_width = name_width.min(self.limits.max_layout_width);
        let has_binary = self
            .files
            .iter()
            .any(|file| matches!(file.counts, Counts::Binary { .. }));
        let max_changes = self
            .files
            .iter()
            .filter_map(|file| match file.counts {
                Counts::Text { added, removed } => Some(added.saturating_add(removed)),
                Counts::Binary { .. } => None,
            })
            .max()
            .unwrap_or_default();
        let count_width = max_changes
            .to_string()
            .len()
            .max(if has_binary { 3 } else { 1 });
        let graph_width = layout.width.saturating_sub(1 + name_width + 6).max(6);

        for (file, path) in self.files.iter().zip(paths) {
            output.append(b" ")?;
            let name_capacity = name_width.saturating_sub(2);
            let shown = truncate_path(&path, name_capacity);
            output.append(shown.as_bytes())?;
            if shown.len() < name_capacity {
                append_spaces(&mut output, name_capacity - shown.len())?;
            }
            output.append(b" | ")?;
            match file.counts {
                Counts::Binary {
                    old_bytes,
                    new_bytes,
                } => {
                    output.append(format!("Bin {old_bytes} -> {new_bytes} bytes").as_bytes())?;
                }
                Counts::Text { added, removed } => {
                    let count = added.saturating_add(removed);
                    if count == 0 {
                        output.append(format!("{count:>count_width$}").as_bytes())?;
                    } else {
                        output.append(format!("{count:>count_width$} ").as_bytes())?;
                        let graph_len = if max_changes <= graph_width as u64 {
                            usize::try_from(count)
                                .unwrap_or(usize::MAX)
                                .min(graph_width)
                        } else {
                            let scaled = u128::from(count) * graph_width as u128;
                            let scaled = usize::try_from(
                                (scaled + u128::from(max_changes) / 2) / u128::from(max_changes),
                            )
                            .unwrap_or(graph_width)
                            .clamp(1, graph_width);
                            if added > 0 && removed > 0 {
                                scaled.max(2).min(graph_width)
                            } else {
                                scaled
                            }
                        };
                        let plus = graph_added(added, removed, graph_len, count)?;
                        append_repeat(&mut output, b'+', plus)?;
                        append_repeat(&mut output, b'-', graph_len.saturating_sub(plus))?;
                    }
                }
            }
            output.append(b"\n")?;
        }
        append_summary(&mut output, &self.files)?;
        Ok(output.bytes)
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
    /// Maximum requested human-stat width; default 4096 columns.
    pub max_layout_width: usize,
}

impl Limits {
    /// Sets [`Self::max_layout_width`].
    #[must_use]
    pub fn with_max_layout_width(mut self, max_layout_width: usize) -> Self {
        self.max_layout_width = max_layout_width;
        self
    }

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
            max_layout_width: 4096,
        }
    }
}

/// Explicit width for [`Diffstat::stat`]. The default matches Git's 80-column layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatLayout {
    /// Requested row width in columns; very narrow layouts retain Git's minimum readable width.
    pub width: usize,
}

impl Default for StatLayout {
    fn default() -> Self {
        Self { width: 80 }
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

fn truncate_path(path: &str, capacity: usize) -> String {
    if path.len() <= capacity {
        return path.to_owned();
    }
    let Some((_, basename)) = path.rsplit_once('/') else {
        return format!(
            "...{}",
            path.chars()
                .rev()
                .take(capacity.saturating_sub(3))
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
    };
    let suffix = format!(".../{basename}");
    if suffix.len() <= capacity {
        return suffix;
    }
    let basename_only = format!("...{basename}");
    if basename_only.len() <= capacity.saturating_add(1) {
        return basename_only;
    }
    let tail = path
        .chars()
        .rev()
        .take(capacity.saturating_sub(3))
        .collect::<String>();
    format!("...{}", tail.chars().rev().collect::<String>())
}

fn graph_added(added: u64, removed: u64, width: usize, count: u64) -> Result<usize, Error> {
    if count <= width as u64 {
        return Ok(usize::try_from(added).unwrap_or(usize::MAX).min(width));
    }
    let scaled = (u128::from(added) * width as u128 + u128::from(count / 2)) / u128::from(count);
    let mut plus = usize::try_from(scaled).unwrap_or(width).min(width);
    if added > 0 && plus == 0 {
        plus = 1;
    }
    if removed > 0 && plus == width {
        plus = width.saturating_sub(1);
    }
    Ok(plus)
}

fn append_spaces(output: &mut Output, count: usize) -> Result<(), Error> {
    const SPACES: &[u8] = b"                                                                ";
    let mut remaining = count;
    while remaining != 0 {
        let chunk = remaining.min(SPACES.len());
        output.append(&SPACES[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn append_repeat(output: &mut Output, byte: u8, count: usize) -> Result<(), Error> {
    let repeats = [byte; 64];
    let mut remaining = count;
    while remaining != 0 {
        let chunk = remaining.min(repeats.len());
        output.append(&repeats[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn append_summary(output: &mut Output, files: &[FileStat]) -> Result<(), Error> {
    let mut added = 0u64;
    let mut removed = 0u64;
    for file in files {
        if let Counts::Text {
            added: file_added,
            removed: file_removed,
        } = file.counts
        {
            added = added.saturating_add(file_added);
            removed = removed.saturating_add(file_removed);
        }
    }
    let count = files.len();
    output
        .append(format!(" {count} file{} changed", if count == 1 { "" } else { "s" }).as_bytes())?;
    if added == 0 && removed == 0 {
        output.append(b", 0 insertions(+), 0 deletions(-)\n")?;
    } else {
        if added > 0 {
            output.append(
                format!(
                    ", {added} insertion{}(+)",
                    if added == 1 { "" } else { "s" }
                )
                .as_bytes(),
            )?;
        }
        if removed > 0 {
            output.append(
                format!(
                    ", {removed} deletion{}(-)",
                    if removed == 1 { "" } else { "s" }
                )
                .as_bytes(),
            )?;
        }
        output.append(b"\n")?;
    }
    Ok(())
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
