//! Line-ending statistics and conversion.

use std::borrow::Cow;

/// Counts Git's text heuristic and the line-ending conversions consult.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Stats {
    nul: usize,
    lone_cr: usize,
    pub(super) crlf: usize,
    pub(super) lone_lf: usize,
    printable: usize,
    nonprintable: usize,
}

impl Stats {
    pub(super) fn new(bytes: &[u8]) -> Self {
        let mut stats = Self::default();
        // A trailing DOS end-of-file marker is not counted.
        let counted = bytes.strip_suffix(b"\x1a").unwrap_or(bytes);
        let mut previous_cr = false;
        for &byte in counted {
            if previous_cr && byte != b'\n' {
                stats.lone_cr += 1;
            }
            match byte {
                b'\r' => {}
                b'\n' if previous_cr => stats.crlf += 1,
                b'\n' => stats.lone_lf += 1,
                0 => stats.nul += 1,
                // Backspace, tab, form feed and escape count as printable, as do all bytes from
                // space up, except DEL.
                0x08 | b'\t' | 0x0c | 0x1b => stats.printable += 1,
                0x7f => stats.nonprintable += 1,
                _ if byte < 0x20 => stats.nonprintable += 1,
                _ => stats.printable += 1,
            }
            previous_cr = byte == b'\r';
        }
        if previous_cr {
            stats.lone_cr += 1;
        }
        stats
    }

    /// Whether `text=auto` treats the content as binary: it has a NUL or a CR not followed by LF,
    /// or more than one nonprintable byte per 128 printable ones.
    pub(super) fn binary(&self) -> bool {
        self.nul > 0 || self.lone_cr > 0 || (self.printable >> 7) < self.nonprintable
    }

    /// Whether the content contains any CR.
    pub(super) fn has_cr(&self) -> bool {
        self.crlf > 0 || self.lone_cr > 0
    }
}

/// Replaces each CRLF with LF; a CR not followed by LF is kept.
pub(super) fn crlf_to_lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().peekable();
    while let Some(&byte) = iter.next() {
        if byte != b'\r' || iter.peek() != Some(&&b'\n') {
            out.push(byte);
        }
    }
    out
}

/// Replaces each LF not preceded by CR with CRLF.
pub(super) fn lf_to_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    let mut previous = 0;
    for &byte in bytes {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        previous = byte;
    }
    out
}

/// Applies `convert` when `needed`, borrowing otherwise.
pub(super) fn convert_if(
    bytes: Cow<'_, [u8]>,
    needed: bool,
    convert: fn(&[u8]) -> Vec<u8>,
) -> Cow<'_, [u8]> {
    if needed {
        Cow::Owned(convert(&bytes))
    } else {
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_matches_observed_git_thresholds() {
        // One nonprintable byte needs 128 printable ones; two need 256 (observed with `git add`
        // under `text=auto`).
        let with = |control: usize, printable: usize| {
            let mut bytes = vec![1; control];
            bytes.extend(std::iter::repeat_n(b'a', printable));
            bytes.extend_from_slice(b"\r\n");
            Stats::new(&bytes).binary()
        };
        assert!(with(1, 127));
        assert!(!with(1, 128));
        assert!(with(2, 255));
        assert!(!with(2, 256));
        assert!(Stats::new(b"a\rb\n").binary());
        assert!(Stats::new(b"a\r\nb\r").binary());
        assert!(!Stats::new(b"a\r\n\x1a").binary());
        assert!(!Stats::new(b"").binary());
    }

    #[test]
    fn conversions_keep_lone_cr() {
        assert_eq!(crlf_to_lf(b"a\r\nb\rc\r"), b"a\nb\rc\r");
        assert_eq!(lf_to_crlf(b"\na\r\nb\rc\n"), b"\r\na\r\nb\rc\r\n");
    }
}
