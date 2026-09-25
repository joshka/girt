//! Read-only discovery of the object-directory graph.
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::ObjectReadError;

/// Aggregate bounds for a primary store and its transitive `info/alternates` graph.
///
/// Discovery is iterative and visits canonical directories once. Limits include the primary
/// store, repeated/missing path records and all metadata read. No process environment is read.
#[derive(Debug, Clone, Copy)]
pub struct AlternateLimits {
    /// Maximum distinct existing directories, including the primary (default 1,024).
    pub max_stores: usize,
    /// Maximum nonempty, noncomment path records, including aliases (default 4,096).
    pub max_entries: usize,
    /// Maximum aggregate alternates-file bytes (default 1 MiB).
    pub max_bytes: usize,
    /// Maximum cumulative input and canonical path bytes, including the primary (default 4 MiB).
    ///
    /// Charges native encoded path lengths before joining each record to its parent. This
    /// bounds pending path allocation even when a long parent has many children.
    pub max_path_bytes: usize,
}

impl Default for AlternateLimits {
    fn default() -> Self {
        Self {
            max_stores: 1024,
            max_entries: 4096,
            max_bytes: 1024 * 1024,
            max_path_bytes: 4 * 1024 * 1024,
        }
    }
}

pub(super) fn discover(
    root: &Path,
    limits: AlternateLimits,
) -> Result<Vec<PathBuf>, ObjectReadError> {
    let mut paths = limits.max_path_bytes;
    charge_path(&mut paths, root.as_os_str().len())?;
    let mut pending = vec![root.to_owned()];
    let mut seen = HashSet::new();
    let mut stores = Vec::new();
    let mut bytes = limits.max_bytes;
    let mut entries = limits.max_entries;
    while let Some(path) = pending.pop() {
        let directory = match fs::canonicalize(&path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !stores.is_empty() => continue,
            Err(source) => return Err(ObjectReadError::Path { path, source }),
        };
        if seen.contains(&directory) {
            continue;
        }
        if stores.len() == limits.max_stores {
            return Err(ObjectReadError::Limit("alternate store count"));
        }
        charge_path(&mut paths, directory.as_os_str().len())?;
        seen.insert(directory.clone());
        let source = directory.join("info/alternates");
        let data = read_metadata(&source, &mut bytes)?;
        let mut children = Vec::new();
        for record in data.split(|byte| *byte == b'\n') {
            if record.is_empty() || record.starts_with(b"#") {
                continue;
            }
            entries = entries
                .checked_sub(1)
                .ok_or(ObjectReadError::Limit("alternate path count"))?;
            let path = parse_path(record).map_err(|reason| ObjectReadError::Alternate {
                path: source.clone(),
                reason,
            })?;
            let base = if path.is_absolute() {
                0
            } else {
                directory.as_os_str().len().saturating_add(1)
            };
            charge_path(&mut paths, base.saturating_add(path.as_os_str().len()))?;
            children.push(directory.join(path));
        }
        // A stack preserves depth-first file order without recursive stack growth.
        pending.extend(children.into_iter().rev());
        stores.push(directory);
    }
    Ok(stores)
}

fn read_metadata(path: &Path, remaining: &mut usize) -> Result<Vec<u8>, ObjectReadError> {
    let at_path = |source| ObjectReadError::Path {
        path: path.to_owned(),
        source,
    };
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(at_path(source)),
    };
    if file.metadata().map_err(at_path)?.len() > *remaining as u64 {
        return Err(ObjectReadError::Limit("alternate metadata bytes"));
    }
    let mut data = Vec::new();
    file.take((*remaining as u64).saturating_add(1))
        .read_to_end(&mut data)
        .map_err(at_path)?;
    if data.len() > *remaining {
        return Err(ObjectReadError::Limit("alternate metadata bytes"));
    }
    *remaining -= data.len();
    Ok(data)
}

fn parse_path(record: &[u8]) -> Result<PathBuf, &'static str> {
    let bytes = if record.starts_with(b"\"") {
        unquote(record)?
    } else {
        record.to_vec()
    };
    if bytes.is_empty() || bytes.contains(&0) {
        return Err("empty path or NUL byte");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(&bytes)
            .map(PathBuf::from)
            .map_err(|_| "non-UTF-8 native path")
    }
}

fn unquote(record: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut result = Vec::new();
    let mut input = record[1..].iter().copied();
    while let Some(byte) = input.next() {
        match byte {
            b'"' => {
                if input.next().is_some() {
                    return Err("trailing bytes after quoted path");
                }
                return Ok(result);
            }
            b'\\' => {
                let escaped = input.next().ok_or("unfinished path escape")?;
                let byte = match escaped {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'0'..=b'3' => {
                        let second = input
                            .next()
                            .filter(|b| (b'0'..=b'7').contains(b))
                            .ok_or("invalid octal path escape")?;
                        let third = input
                            .next()
                            .filter(|b| (b'0'..=b'7').contains(b))
                            .ok_or("invalid octal path escape")?;
                        (escaped - b'0') * 64 + (second - b'0') * 8 + (third - b'0')
                    }
                    _ => return Err("invalid path escape"),
                };
                result.push(byte);
            }
            _ => result.push(byte),
        }
    }
    Err("unterminated quoted path")
}

fn charge_path(remaining: &mut usize, bytes: usize) -> Result<(), ObjectReadError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(ObjectReadError::Limit("alternate resolved path bytes"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain(b"../objects", b"../objects")]
    #[case::spaces(b"a b", b"a b")]
    #[case::quoted(b"\"a b\"", b"a b")]
    #[case::octal(b"\"a\\040b\"", b"a b")]
    #[case::escaped(b"\"a\\nb\"", b"a\nb")]
    #[case::cr(b"a\r", b"a\r")]
    fn preserves_path_bytes(#[case] input: &[u8], #[case] expected: &[u8]) {
        assert_eq!(
            parse_path(input).unwrap().as_os_str().as_encoded_bytes(),
            expected
        );
    }

    #[test]
    fn decodes_full_byte_range_without_intermediate_overflow() {
        assert_eq!(unquote(b"\"\\000\\377\"").unwrap(), [0, 255]);
    }

    #[rstest]
    #[case::suffix(b"\"a\"trailing")]
    #[case::nul(b"a\0b")]
    #[case::quoted_nul(b"\"a\\000b\"")]
    #[case::empty(b"\"\"")]
    #[case::unfinished(b"\"a")]
    #[case::escape(b"\"a\\q\"")]
    #[case::octal(b"\"a\\400\"")]
    fn rejects_malformed_paths(#[case] input: &[u8]) {
        assert!(parse_path(input).is_err());
    }
}
