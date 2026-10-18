use std::io::Write as _;

use super::Error;
use crate::display::DateFormat;
use crate::{Commit, IdentityRef, ObjectId, Objects};

/// Expands `$Format:<placeholders>$` in files with the `export-subst` attribute.
pub(super) struct Subst<'a> {
    pub(super) objects: &'a Objects,
    pub(super) id: ObjectId,
    pub(super) commit: &'a Commit,
    pub(super) abbrev: usize,
}

impl Subst<'_> {
    /// Replaces every `$Format:…$` in `content`; text outside them is unchanged.
    pub(super) fn expand(&self, path: &[u8], content: &[u8]) -> Result<Vec<u8>, Error> {
        const OPEN: &[u8] = b"$Format:";
        let mut out = Vec::with_capacity(content.len());
        let mut rest = content;
        while let Some(start) = find(rest, OPEN) {
            let after = rest.get(start + OPEN.len()..).unwrap_or_default();
            let Some(end) = after.iter().position(|&byte| byte == b'$') else {
                break;
            };
            out.extend_from_slice(rest.get(..start).unwrap_or_default());
            self.format(path, after.get(..end).unwrap_or_default(), &mut out)?;
            rest = after.get(end + 1..).unwrap_or_default();
        }
        out.extend_from_slice(rest);
        Ok(out)
    }

    /// Writes one pretty format, as `git log --format` would for the commit.
    fn format(&self, path: &[u8], format: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        let unsupported = |placeholder: &[u8]| Error::Placeholder {
            path: path.to_vec(),
            placeholder: String::from_utf8_lossy(placeholder).into_owned(),
        };
        let mut index = 0;
        while let Some(&byte) = format.get(index) {
            index += 1;
            if byte != b'%' {
                out.push(byte);
                continue;
            }
            let Some(&code) = format.get(index) else {
                return Err(unsupported(b"%"));
            };
            index += 1;
            match code {
                b'%' => out.push(b'%'),
                b'n' => out.push(b'\n'),
                b'H' => out.extend_from_slice(self.id.to_string().as_bytes()),
                b'h' => out.extend_from_slice(self.abbreviated(self.id)?.as_bytes()),
                b'T' => out.extend_from_slice(self.commit.tree().to_string().as_bytes()),
                b't' => out.extend_from_slice(self.abbreviated(self.commit.tree())?.as_bytes()),
                b'P' | b'p' => {
                    for (position, &parent) in self.commit.parents().iter().enumerate() {
                        if position > 0 {
                            out.push(b' ');
                        }
                        let text = if code == b'P' {
                            parent.to_string()
                        } else {
                            self.abbreviated(parent)?
                        };
                        out.extend_from_slice(text.as_bytes());
                    }
                }
                b's' => out.extend_from_slice(&self.commit.subject()),
                b'B' => out.extend_from_slice(self.commit.message()),
                b'a' | b'c' => {
                    let Some(&field) = format.get(index) else {
                        return Err(unsupported(format.get(index - 2..).unwrap_or_default()));
                    };
                    index += 1;
                    let identity = if code == b'a' {
                        self.commit.author()
                    } else {
                        self.commit.committer()
                    }
                    .map_err(|source| Error::Commit(Box::new(source)))?;
                    let placeholder = format.get(index - 3..index).unwrap_or_default();
                    let Some(identity) = identity else {
                        return Err(unsupported(placeholder));
                    };
                    write_identity(identity, field, out)
                        .ok_or_else(|| unsupported(placeholder))?
                        .map_err(|source| Error::Commit(Box::new(source)))?;
                }
                _ => {
                    return Err(unsupported(
                        format.get(index - 2..index).unwrap_or_default(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn abbreviated(&self, id: ObjectId) -> Result<String, Error> {
        Ok(self
            .objects
            .abbreviate(id, self.abbrev)
            .map_err(|source| Error::Object(Box::new(source)))?
            .to_string())
    }
}

/// Writes `%an`, `%ae`, `%ad`, `%ai` or `%at` (and the committer forms); `None` for a field
/// girt does not format.
fn write_identity(
    identity: IdentityRef<'_>,
    field: u8,
    out: &mut Vec<u8>,
) -> Option<Result<(), crate::CommitError>> {
    let date = |format| identity.format_date(format).map(|date| date.to_string());
    let result = match field {
        b'n' => {
            out.extend_from_slice(identity.name);
            Ok(())
        }
        b'e' => {
            out.extend_from_slice(identity.email);
            Ok(())
        }
        b'd' => date(DateFormat::Default).map(|date| out.extend_from_slice(date.as_bytes())),
        b'i' => date(DateFormat::Iso).map(|date| out.extend_from_slice(date.as_bytes())),
        b't' => identity
            .date()
            .map(|date| drop(write!(out, "{}", date.seconds))),
        _ => return None,
    };
    Some(result)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
