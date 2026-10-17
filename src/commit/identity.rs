use super::{CommitError, Signature};
use crate::display::{DateFormat, FormattedDate};

/// Borrowed identity components and uninterpreted date bytes from an imported object.
///
/// The first `<` and following `>` delimit the email. Trailing ASCII whitespace before `<`
/// is excluded from the name; all other bytes remain intact, including non-UTF-8 bytes.
/// Date failures never prevent inspection of the name or email.
///
/// ```
/// use girt::IdentityRef;
///
/// let identity = IdentityRef::parse(b"A <a@example.invalid> 60 +0130")?;
/// assert_eq!(identity.email, b"a@example.invalid");
/// let date = identity.date()?;
/// assert_eq!(date.seconds, 60);
/// assert_eq!(date.offset_minutes, 90);
///
/// let malformed = IdentityRef::parse(b"A <a@example.invalid> unknown")?;
/// assert!(malformed.date().is_err());
/// assert_eq!(malformed.date_bytes, b" unknown");
/// # Ok::<(), girt::CommitError>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdentityRef<'a> {
    /// Name bytes, without trailing delimiter whitespace.
    pub name: &'a [u8],
    /// Email bytes between the selected brackets.
    pub email: &'a [u8],
    /// Exact suffix after `>`, including whitespace and any malformed date text.
    pub date_bytes: &'a [u8],
}

/// A numerically interpreted Git identity date, independent of canonical construction policy.
///
/// Use this interpretation for arithmetic or display after deciding how the application presents
/// pre-epoch dates and unusual offsets. The raw spelling remains on [`IdentityRef`]; converting
/// dates is not a byte-preserving edit.
///
/// ```
/// use girt::IdentityRef;
/// let identity = IdentityRef::parse(b"A <a@example.com> -1 -0000")?;
/// let date = identity.date()?;
/// assert_eq!((date.seconds, date.offset_minutes), (-1, 0));
/// assert!(identity.date_bytes.ends_with(b"-0000"));
/// # Ok::<(), girt::CommitError>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdentityDate {
    /// Signed seconds since the Unix epoch; no display fallback is applied.
    pub seconds: i64,
    /// Signed offset in minutes. Noncanonical hours and minutes are interpreted arithmetically.
    pub offset_minutes: i16,
}

impl IdentityDate {
    /// Renders this numeric date in Git's `--date=default` or `--date=iso` layout.
    ///
    /// The zone prints as `±HHMM` from [`offset_minutes`](Self::offset_minutes). This replaces
    /// hand-written calendar arithmetic over [`seconds`](Self::seconds) and
    /// [`offset_minutes`](Self::offset_minutes), which remain the explicit values.
    ///
    /// This is deliberately a numeric formatter: it canonicalizes offsets (`+0090` becomes
    /// `+0130`) and renders pre-epoch local times as proleptic Gregorian dates. It cannot recover
    /// recorded zone digits from an interpreted offset. Use [`IdentityRef::format_date`] when
    /// displaying imported identities with Git's zone and pre-epoch policy. This distinction is
    /// the resolution of [#422](https://github.com/joshka/girt/issues/422).
    ///
    /// ```
    /// use girt::IdentityRef;
    /// use girt::display::DateFormat;
    ///
    /// let date = IdentityRef::parse(b"A <a@example.com> 1696320000 -0800")?.date()?;
    /// assert_eq!(
    ///     date.format(DateFormat::Default).to_string(),
    ///     "Tue Oct 3 00:00:00 2023 -0800"
    /// );
    /// assert_eq!(
    ///     date.format(DateFormat::Iso).to_string(),
    ///     "2023-10-03 00:00:00 -0800"
    /// );
    /// # Ok::<(), girt::CommitError>(())
    /// ```
    #[must_use]
    pub fn format(self, format: DateFormat) -> FormattedDate {
        FormattedDate::new(self, format)
    }
}

impl From<&Signature> for IdentityDate {
    /// Copies the signature's seconds and offset, for example to [`format`](Self::format) it.
    fn from(signature: &Signature) -> Self {
        Self {
            seconds: signature.seconds,
            offset_minutes: signature.offset_minutes,
        }
    }
}

/// Splits an optional sign and at least one decimal digit from the following bytes.
fn signed_decimal(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let sign = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let digits = bytes
        .get(sign..)?
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    (digits > 0).then(|| bytes.split_at_checked(sign + digits))?
}

impl<'a> IdentityRef<'a> {
    /// Borrows name, email and date bytes without interpreting the date.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] if either selected bracket is absent.
    ///
    /// ```
    /// use girt::IdentityRef;
    /// let identity = IdentityRef::parse(b"A <a@example.com> invalid-date")?;
    /// assert_eq!(identity.name, b"A");
    /// assert_eq!(identity.email, b"a@example.com");
    /// assert!(identity.date().is_err());
    /// # Ok::<(), girt::CommitError>(())
    /// ```
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CommitError> {
        let (name, rest) =
            crate::bytes::split_once(bytes, b'<').ok_or(CommitError::InvalidSignature)?;
        let (email, date_bytes) =
            crate::bytes::split_once(rest, b'>').ok_or(CommitError::InvalidSignature)?;
        Ok(Self {
            name: name.trim_ascii_end(),
            email,
            date_bytes,
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
    ///
    /// ```
    /// use girt::IdentityRef;
    /// let date = IdentityRef::parse(b"A <a@example.com> 60 -0130")?.date()?;
    /// assert_eq!(date.seconds, 60);
    /// assert_eq!(date.offset_minutes, -90);
    /// # Ok::<(), girt::CommitError>(())
    /// ```
    pub fn date(self) -> Result<IdentityDate, CommitError> {
        self.date_parts().map(|(date, _)| date)
    }

    fn date_parts(self) -> Result<(IdentityDate, i32), CommitError> {
        let bytes = self.date_bytes.trim_ascii_start();
        let (seconds, zone) = signed_decimal(bytes).ok_or(CommitError::InvalidDate)?;
        let seconds = std::str::from_utf8(seconds)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(CommitError::InvalidDate)?;
        let zone = zone.trim_ascii_start();
        if !matches!(zone.first(), Some(b'+' | b'-')) {
            return Err(CommitError::InvalidDate);
        }
        let (hhmm, _) = signed_decimal(zone).ok_or(CommitError::InvalidDate)?;
        let hhmm: i32 = std::str::from_utf8(hhmm)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(CommitError::InvalidDate)?;
        let minutes = (hhmm / 100)
            .checked_mul(60)
            .and_then(|v| v.checked_add(hhmm % 100))
            .and_then(|v| i16::try_from(v).ok())
            .ok_or(CommitError::InvalidDate)?;
        Ok((
            IdentityDate {
                seconds,
                offset_minutes: minutes,
            },
            hhmm,
        ))
    }

    /// Formats an imported date with Git's recorded zone and local-epoch policy.
    ///
    /// Unlike [`IdentityDate::format`], this preserves the numeric HHMM zone: `+0090` shifts
    /// the time by 90 minutes and prints `+0090`. Leading zeroes beyond four digits and negative
    /// zero are normalized, as in `git log --format=%ad --date=default/iso` (Git 2.56).
    /// Default output omits the zone for recorded `-0001`, as Git does; ISO output includes it.
    /// Calendar arithmetic remains exact across the supported `i64` seconds range; Git's
    /// platform-dependent extreme-year wrapping and overflow fallback are not reproduced.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidDate`] for the interpretation failures in [`Self::date`]
    /// (including offsets outside `i16` minutes), signed seconds spellings, or a local time
    /// before 1970-01-01. Git prints an empty date for signed seconds; this API explicitly
    /// refuses them. No formatted value is returned on failure.
    ///
    /// ```
    /// use girt::IdentityRef;
    /// use girt::display::DateFormat;
    /// let identity = IdentityRef::parse(b"A <a@example.com> 0 +0090")?;
    /// assert_eq!(
    ///     identity.format_date(DateFormat::Iso)?.to_string(),
    ///     "1970-01-01 01:30:00 +0090"
    /// );
    /// # Ok::<(), girt::CommitError>(())
    /// ```
    pub fn format_date(self, format: DateFormat) -> Result<FormattedDate, CommitError> {
        if matches!(
            self.date_bytes.trim_ascii_start().first(),
            Some(b'+' | b'-')
        ) {
            return Err(CommitError::InvalidDate);
        }
        let (date, hhmm) = self.date_parts()?;
        let local = i128::from(date.seconds) + i128::from(date.offset_minutes) * 60;
        if local < 0 {
            return Err(CommitError::InvalidDate);
        }
        Ok(FormattedDate::new(date, format).with_recorded_zone(hhmm))
    }

    /// Copies identity bytes and interprets the date for explicit construction.
    ///
    /// # Errors
    ///
    /// Returns the date interpretation failures from [`Self::date`]. Construction validation is
    /// still separate; this conversion does not enforce canonical identity or offset rules.
    ///
    /// ```
    /// use girt::{IdentityRef, Signature};
    /// let imported = b"Writer <writer@example.com> 60 -0130";
    /// let identity = IdentityRef::parse(imported)?;
    /// let signature = identity.signature()?;
    /// assert_eq!(signature.seconds, 60);
    /// assert_eq!(signature.offset_minutes, -90);
    /// assert_eq!(signature.email, b"writer@example.com");
    /// # Ok::<(), girt::CommitError>(())
    /// ```
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
    #[case::west_of_epoch(b"0 -0001")]
    #[case::one_second_before(b"59 -0001")]
    #[case::negative_instant(b"-1 +0000")]
    #[case::negative_instant_positive_local(b"-60 +0100")]
    #[case::plus_seconds(b"+5 +0000")]
    #[case::offset_overflow(b"1 +60000")]
    #[case::malformed(b"unknown")]
    fn refuses_unformattable_imported_dates(#[case] bytes: &[u8]) {
        let identity = IdentityRef {
            name: b"A",
            email: b"a",
            date_bytes: bytes,
        };
        assert!(matches!(
            identity.format_date(DateFormat::Default),
            Err(CommitError::InvalidDate)
        ));
        assert!(matches!(
            identity.format_date(DateFormat::Iso),
            Err(CommitError::InvalidDate)
        ));
        assert_eq!(identity.date_bytes, bytes);
    }

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
    #[case::sign_only(b"-")]
    #[case::sign_before_zone(b"- +0000")]
    #[case::zone_sign_only(b"1 +")]
    #[case::zone_sign_at_end(b"1 -")]
    fn preserves_bytes_after_interpretation_error(#[case] bytes: &[u8]) {
        let input = [b"A\xff<a\xfe>".as_slice(), bytes].concat();
        let identity = IdentityRef::parse(&input).unwrap();
        assert_eq!(identity.date(), Err(CommitError::InvalidDate));
        assert_eq!(
            (identity.name, identity.email, identity.date_bytes),
            (b"A\xff".as_slice(), b"a\xfe".as_slice(), bytes)
        );
    }

    #[rstest]
    #[case::no_brackets(b"A a 1 +0000".as_slice())]
    #[case::unclosed(b"A <a 1 +0000")]
    #[case::open_at_end(b"A <")]
    #[case::close_before_open(b"A >a< 1 +0000")]
    fn rejects_identity_without_brackets(#[case] bytes: &[u8]) {
        assert_eq!(
            IdentityRef::parse(bytes),
            Err(CommitError::InvalidSignature)
        );
    }
}
