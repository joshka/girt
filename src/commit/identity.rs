use super::{CommitError, Signature};

/// Borrowed identity components and uninterpreted date bytes from an imported object.
///
/// The first `<` and following `>` delimit the email. Trailing ASCII whitespace before `<`
/// is excluded from the name; all other bytes remain intact, including non-UTF-8 bytes.
/// Date failures never prevent inspection of the name or email.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityRef<'a> {
    /// Name bytes, without trailing delimiter whitespace.
    pub name: &'a [u8],
    /// Email bytes between the selected brackets.
    pub email: &'a [u8],
    /// Exact suffix after `>`, including whitespace and any malformed date text.
    pub date_bytes: &'a [u8],
}

/// A numerically interpreted Git identity date, independent of canonical construction policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityDate {
    /// Signed seconds since the Unix epoch; no display fallback is applied.
    pub seconds: i64,
    /// Signed offset in minutes. Noncanonical hours and minutes are interpreted arithmetically.
    pub offset_minutes: i16,
}

impl<'a> IdentityRef<'a> {
    /// Borrows name, email and date bytes without interpreting the date.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] if either selected bracket is absent.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CommitError> {
        let open = bytes
            .iter()
            .position(|&b| b == b'<')
            .ok_or(CommitError::InvalidSignature)?;
        let close = bytes[open + 1..]
            .iter()
            .position(|&b| b == b'>')
            .map(|i| i + open + 1)
            .ok_or(CommitError::InvalidSignature)?;
        Ok(Self {
            name: bytes[..open].trim_ascii_end(),
            email: &bytes[open + 1..close],
            date_bytes: &bytes[close + 1..],
        })
    }

    /// Interprets signed decimal seconds and a signed decimal HHMM offset.
    ///
    /// ASCII whitespace between seconds and zone is optional. Leading zeros, short offsets,
    /// noncanonical hours/minutes, and text after the zone’s decimal prefix are accepted. Negative
    /// dates are preserved even where Git's display reports no date. No epoch or overflow
    /// fallback is invented.
    ///
    /// # Errors
    ///
    /// Missing/malformed tokens, seconds outside `i64`, and offsets outside `i16` minutes return
    /// [`CommitError::InvalidDate`]. Raw date bytes remain available after any failure.
    pub fn date(self) -> Result<IdentityDate, CommitError> {
        let bytes = self.date_bytes.trim_ascii_start();
        let sign = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
        let end = sign
            + bytes[sign..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
        if end == sign {
            return Err(CommitError::InvalidDate);
        }
        let seconds = std::str::from_utf8(&bytes[..end])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(CommitError::InvalidDate)?;
        let zone = bytes[end..].trim_ascii_start();
        if !matches!(zone.first(), Some(b'+' | b'-')) {
            return Err(CommitError::InvalidDate);
        }
        let end = 1 + zone[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if end == 1 {
            return Err(CommitError::InvalidDate);
        }
        let hhmm: i32 = std::str::from_utf8(&zone[..end])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(CommitError::InvalidDate)?;
        let minutes = (hhmm / 100)
            .checked_mul(60)
            .and_then(|v| v.checked_add(hhmm % 100))
            .and_then(|v| i16::try_from(v).ok())
            .ok_or(CommitError::InvalidDate)?;
        Ok(IdentityDate {
            seconds,
            offset_minutes: minutes,
        })
    }

    /// Copies identity bytes and interprets the date for explicit construction.
    ///
    /// # Errors
    ///
    /// Returns the date interpretation failures from [`Self::date`]. Construction validation is
    /// still separate; this conversion does not enforce canonical identity or offset rules.
    pub fn signature(self) -> Result<Signature, CommitError> {
        let date = self.date()?;
        Ok(Signature {
            name: self.name.to_vec(),
            email: self.email.to_vec(),
            seconds: date.seconds,
            offset_minutes: date.offset_minutes,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::pre_epoch(b"-1 +0000", -1, 0)]
    #[case::minimum(b"-9223372036854775808 -2359", i64::MIN, -1439)]
    #[case::maximum(b"9223372036854775807 +2359", i64::MAX, 1439)]
    #[case::padded(b"+00042 -0000", 42, 0)]
    #[case::tabs(b"\t1\t+0100", 1, 60)]
    #[case::short(b"1 +1", 1, 1)]
    #[case::hours(b"1 +2400", 1, 1440)]
    #[case::minutes(b"1 -0060", 1, -60)]
    #[case::trailing(b"1 +0000 text", 1, 0)]
    #[case::suffix(b"1 +01foo", 1, 1)]
    #[case::adjacent(b"1+0100", 1, 60)]
    fn interprets_numbers_without_construction_policy(
        #[case] bytes: &[u8],
        #[case] seconds: i64,
        #[case] offset_minutes: i16,
    ) {
        let identity = IdentityRef {
            name: b"A",
            email: b"a",
            date_bytes: bytes,
        };
        assert_eq!(
            identity.date(),
            Ok(IdentityDate {
                seconds,
                offset_minutes
            })
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::missing_zone(b"1")]
    #[case::overflow(b"9223372036854775808 +0000")]
    #[case::underflow(b"-9223372036854775809 +0000")]
    #[case::text(b"now +0000")]
    #[case::unsigned(b"1 0000")]
    #[case::zone_text(b"1 +x000")]
    #[case::zone_overflow(b"1 +99999999999999999999999")]
    fn preserves_bytes_after_interpretation_error(#[case] bytes: &[u8]) {
        let input = [b"A\xff<a\xfe>".as_slice(), bytes].concat();
        let identity = IdentityRef::parse(&input).unwrap();
        assert_eq!(identity.date(), Err(CommitError::InvalidDate));
        assert_eq!(
            (identity.name, identity.email, identity.date_bytes),
            (b"A\xff".as_slice(), b"a\xfe".as_slice(), bytes)
        );
    }
}
