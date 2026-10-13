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

#[cfg(test)]
mod tests {
    use super::split_once;

    #[test]
    fn split_once_excludes_delimiter() {
        assert_eq!(split_once(b"a b c", b' '), Some((&b"a"[..], &b"b c"[..])));
        assert_eq!(split_once(b"ab ", b' '), Some((&b"ab"[..], &b""[..])));
        assert_eq!(split_once(b"ab", b' '), None);
    }
}
