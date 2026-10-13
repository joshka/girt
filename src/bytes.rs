//! Panic-free byte splitting shared by the parsers of repository and network input.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]

/// Splits at the first `delimiter`, excluding it from both halves.
pub(crate) fn split_once(bytes: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let position = bytes.iter().position(|&byte| byte == delimiter)?;
    let (before, after) = bytes.split_at_checked(position)?;
    Some((before, after.get(1..)?))
}

/// Splits at the last `delimiter`, excluding it from both halves.
pub(crate) fn rsplit_once(bytes: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let position = bytes.iter().rposition(|&byte| byte == delimiter)?;
    let (before, after) = bytes.split_at_checked(position)?;
    Some((before, after.get(1..)?))
}

/// A configuration key's section, optional subsection and variable name.
pub(crate) type ConfigKey<'a> = (&'a [u8], Option<&'a [u8]>, &'a [u8]);

/// Splits a configuration key into section, optional subsection and name at its first and last
/// dots.
pub(crate) fn config_key(key: &[u8]) -> Option<ConfigKey<'_>> {
    let (section, rest) = split_once(key, b'.')?;
    Some(match rsplit_once(rest, b'.') {
        Some((subsection, name)) => (section, Some(subsection), name),
        None => (section, None, rest),
    })
}

#[cfg(test)]
mod tests {
    use super::{config_key, split_once};

    #[test]
    fn split_once_excludes_delimiter() {
        assert_eq!(split_once(b"a b c", b' '), Some((&b"a"[..], &b"b c"[..])));
        assert_eq!(split_once(b"ab ", b' '), Some((&b"ab"[..], &b""[..])));
        assert_eq!(split_once(b"ab", b' '), None);
    }

    #[test]
    fn config_key_splits_at_first_and_last_dot() {
        assert_eq!(config_key(b"a.b"), Some((&b"a"[..], None, &b"b"[..])));
        assert_eq!(
            config_key(b"a.x.y.b"),
            Some((&b"a"[..], Some(&b"x.y"[..]), &b"b"[..]))
        );
        assert_eq!(
            config_key(b"a.."),
            Some((&b"a"[..], Some(&b""[..]), &b""[..]))
        );
        assert_eq!(config_key(b"ab"), None);
    }
}
