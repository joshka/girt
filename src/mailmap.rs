//! Canonical names and email addresses for presenting commit identities.
//!
//! A mailmap changes presentation without changing stored signatures or object IDs. Parse one
//! source with [`Mailmap::parse`], append higher-precedence sources with [`Mailmap::extend`], then
//! resolve author or committer bytes with [`Mailmap::resolve`]. Unknown identities are borrowed
//! unchanged. Matching uses ASCII case folding for both names and emails; output preserves bytes.
//!
//! ```
//! use girt::mailmap::Mailmap;
//! let map = Mailmap::parse(b"A. Writer <writer@example.com> <old@example.com>\n");
//! let identity = map.resolve(b"Writer", b"OLD@example.com");
//! assert_eq!(identity.name, b"A. Writer");
//! assert_eq!(identity.email, b"writer@example.com");
//! ```

mod load;
pub use load::Error;

/// An identity selected for display, borrowing either the input or the map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity<'a> {
    /// Display name, without encoding conversion.
    pub name: &'a [u8],
    /// Display email address, without angle brackets.
    pub email: &'a [u8],
}

/// Parsed mailmap entries in source order.
///
/// All four Git line forms are accepted. Blank, comment and malformed lines are ignored.
/// Later entries replace only the canonical components they supply. A matching name-specific
/// entry takes precedence over an email-only entry, regardless of source order.
#[derive(Clone, Debug, Default)]
pub struct Mailmap {
    entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
struct Entry {
    email: Vec<u8>,
    name: Option<Vec<u8>>,
    canonical_name: Option<Vec<u8>>,
    canonical_email: Option<Vec<u8>>,
}

impl Mailmap {
    /// Parses mailmap bytes. Invalid lines are ignored as by `git check-mailmap`.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut map = Self::default();
        map.extend(bytes);
        map
    }

    /// Adds another source, overriding earlier supplied components for the same lookup key.
    pub fn extend(&mut self, bytes: &[u8]) {
        for line in bytes.split(|&b| b == b'\n') {
            if let Some(entry) = parse_line(line) {
                self.entries.push(entry);
            }
        }
    }

    /// Maps an imported signature for display, preserving its exact date bytes.
    pub fn resolve_signature<'a>(
        &'a self,
        signature: crate::IdentityRef<'a>,
    ) -> crate::IdentityRef<'a> {
        let identity = self.resolve(signature.name, signature.email);
        crate::IdentityRef {
            name: identity.name,
            email: identity.email,
            date_bytes: signature.date_bytes,
        }
    }

    /// Resolves one name/email pair without modifying its stored signature.
    ///
    /// Name-specific matches use the original name. Resolution is a single lookup, not recursive.
    pub fn resolve<'a>(&'a self, name: &'a [u8], email: &'a [u8]) -> Identity<'a> {
        let specific = self.entries.iter().any(|entry| {
            entry.email.eq_ignore_ascii_case(email)
                && entry
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
        });
        let mut result = Identity { name, email };
        for entry in &self.entries {
            if !entry.email.eq_ignore_ascii_case(email) {
                continue;
            }
            let matches = match entry.name.as_deref() {
                Some(n) => specific && n.eq_ignore_ascii_case(name),
                None => !specific,
            };
            if matches {
                if let Some(name) = &entry.canonical_name {
                    result.name = name;
                }
                if let Some(email) = &entry.canonical_email {
                    result.email = email;
                }
            }
        }
        result
    }
}

fn parse_line(line: &[u8]) -> Option<Entry> {
    let line = line.trim_ascii();
    if line.starts_with(b"#") {
        return None;
    }
    let (canonical_name, canonical_email, tail) = identity(line)?;
    let canonical_name = (!canonical_name.is_empty()).then(|| canonical_name.to_vec());
    if let Some((name, email, _)) = identity(tail) {
        Some(Entry {
            email: email.to_vec(),
            name: (!name.is_empty()).then(|| name.to_vec()),
            canonical_name,
            canonical_email: Some(canonical_email.to_vec()),
        })
    } else {
        Some(Entry {
            email: canonical_email.to_vec(),
            name: None,
            canonical_name,
            canonical_email: None,
        })
    }
}

fn identity(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let open = bytes.iter().position(|&b| b == b'<')?;
    let end = open + 1 + bytes[open + 1..].iter().position(|&b| b == b'>')?;
    Some((
        bytes[..open].trim_ascii(),
        &bytes[open + 1..end],
        &bytes[end + 1..],
    ))
}
