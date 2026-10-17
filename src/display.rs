//! Git's human-readable renderings of paths and dates.
//!
//! Git stores paths as bytes and dates as seconds with a zone offset. Its commands print them in
//! fixed text forms: [`quote_path`] matches path output under the default `core.quotePath=true`,
//! and [`IdentityRef::format_date`](crate::IdentityRef::format_date) matches recorded dates in
//! `--date=default` and `--date=iso`. [`IdentityDate::format`] formats numeric dates. These are for
//! output that people or Git-compatible tools read; storage and comparison keep the raw values.
//!
//! Related renderings live with their values: [`EntryMode::octal`](crate::EntryMode::octal),
//! [`EntryMode::object_kind`](crate::EntryMode::object_kind),
//! [`ChangeStatus::letter`](crate::ChangeStatus::letter) and
//! [`rewrites::Kind::letter`](crate::rewrites::Kind::letter).
use std::fmt::{self, Write as _};

use crate::IdentityDate;

/// Renders path bytes as Git prints them with `core.quotePath=true`, its default.
///
/// A path made only of printable ASCII other than `"` and `\` is written unchanged. Any other path
/// is wrapped in double quotes, with `"`, `\` and the control bytes `\a`, `\b`, `\t`, `\n`, `\v`,
/// `\f` and `\r` backslash-escaped, and every other byte outside printable ASCII (including DEL
/// and all non-ASCII bytes) written as a three-digit octal escape. The result is always ASCII, so
/// it is lossless for any byte path. Space is not quoted.
///
/// This replaces writing the escape loop by hand; the raw bytes stay available for storage and
/// comparison. `core.quotePath=false`, which leaves non-ASCII bytes unescaped, is not provided.
///
/// ```
/// use girt::display::quote_path;
///
/// assert_eq!(quote_path(b"src/lib.rs").to_string(), "src/lib.rs");
/// assert_eq!(quote_path(b"tab\there").to_string(), r#""tab\there""#);
/// assert_eq!(
///     quote_path("café".as_bytes()).to_string(),
///     r#""caf\303\251""#
/// );
/// ```
#[must_use]
pub fn quote_path(path: &[u8]) -> QuotedPath<'_> {
    QuotedPath(path)
}

/// Path bytes that display in Git's quoted form; see [`quote_path`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuotedPath<'a>(&'a [u8]);

/// Whether Git escapes this byte inside a quoted path.
fn needs_escape(byte: u8) -> bool {
    !(0x20..0x7f).contains(&byte) || byte == b'"' || byte == b'\\'
}

impl fmt::Display for QuotedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.iter().copied().any(needs_escape) {
            // Printable ASCII only, so every byte is its own character.
            return self
                .0
                .iter()
                .try_for_each(|&byte| f.write_char(char::from(byte)));
        }
        f.write_char('"')?;
        for &byte in self.0 {
            match byte {
                b'"' => f.write_str("\\\"")?,
                b'\\' => f.write_str("\\\\")?,
                b'\x07' => f.write_str("\\a")?,
                b'\x08' => f.write_str("\\b")?,
                b'\t' => f.write_str("\\t")?,
                b'\n' => f.write_str("\\n")?,
                b'\x0b' => f.write_str("\\v")?,
                b'\x0c' => f.write_str("\\f")?,
                b'\r' => f.write_str("\\r")?,
                byte if needs_escape(byte) => write!(f, "\\{byte:03o}")?,
                byte => f.write_char(char::from(byte))?,
            }
        }
        f.write_char('"')
    }
}

/// A Git `--date` text format.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DateFormat {
    /// `--date=default`, as in `git log`: `Tue Oct 3 00:00:00 2023 -0800`. The day of the month is
    /// not padded.
    #[default]
    Default,
    /// `--date=iso`: `2023-10-03 00:00:00 -0800`.
    Iso,
}

/// A date in a Git `--date` layout, produced by [`IdentityDate::format`] or
/// [`IdentityRef::format_date`](crate::IdentityRef::format_date).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormattedDate {
    date: IdentityDate,
    format: DateFormat,
    recorded_zone: Option<i32>,
}

impl FormattedDate {
    pub(crate) fn new(date: IdentityDate, format: DateFormat) -> Self {
        Self {
            date,
            format,
            recorded_zone: None,
        }
    }

    pub(crate) fn with_recorded_zone(mut self, hhmm: i32) -> Self {
        self.recorded_zone = Some(hhmm);
        self
    }
}

impl fmt::Display for FormattedDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let offset = i128::from(self.date.offset_minutes);
        // Wide arithmetic keeps every i64 instant and i16 offset exact.
        let local = i128::from(self.date.seconds) + offset * 60;
        let days = local.div_euclid(86_400);
        let time = local.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let (hour, minute, second) = (time / 3600, time / 60 % 60, time % 60);
        let hhmm = self
            .recorded_zone
            .map_or(offset / 60 * 100 + offset % 60, i128::from);
        let sign = if hhmm < 0 { '-' } else { '+' };
        let zone = hhmm.unsigned_abs();
        let (zone_hours, zone_minutes) = (zone / 100, zone % 100);
        match self.format {
            DateFormat::Default => {
                let weekday = WEEKDAYS[usize::try_from(days.rem_euclid(7)).unwrap_or(0)];
                let month = MONTHS[usize::try_from(month - 1).unwrap_or(0)];
                write!(
                    f,
                    "{weekday} {month} {day} {hour:02}:{minute:02}:{second:02} {year}"
                )?;
                // Git's default presentation treats numeric HHMM -1 as an absent zone.
                if self.recorded_zone == Some(-1) {
                    return Ok(());
                }
                write!(f, " {sign}{zone_hours:02}{zone_minutes:02}")
            }
            DateFormat::Iso => write!(
                f,
                "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} \
                 {sign}{zone_hours:02}{zone_minutes:02}"
            ),
        }
    }
}

/// Converts days since 1970-01-01 to a proleptic Gregorian (year, month, day).
fn civil_from_days(days: i128) -> (i128, i128, i128) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i128::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain(b"dir/file.txt", "dir/file.txt")]
    #[case::space(b"a b", "a b")]
    #[case::empty(b"", "")]
    #[case::quote(b"a\"b", r#""a\"b""#)]
    #[case::backslash(b"a\\b", r#""a\\b""#)]
    #[case::controls(b"\x07\x08\t\n\x0b\x0c\r", r#""\a\b\t\n\v\f\r""#)]
    #[case::octal_control(b"\x01\x1f", r#""\001\037""#)]
    #[case::delete(b"\x7f", r#""\177""#)]
    #[case::non_ascii(b"\xff/\x80", r#""\377/\200""#)]
    fn quotes_paths(#[case] path: &[u8], #[case] expected: &str) {
        assert_eq!(quote_path(path).to_string(), expected);
    }

    // Expected values observed with `git log --date=default` and `--date=iso` (Git 2.56).
    #[rstest]
    #[case::epoch(0, 0, "Thu Jan 1 00:00:00 1970 +0000", "1970-01-01 00:00:00 +0000")]
    #[case::numeric_ninety_minutes(
        0,
        90,
        "Thu Jan 1 01:30:00 1970 +0130",
        "1970-01-01 01:30:00 +0130"
    )]
    #[case::half_hour(
        1_700_000_000,
        330,
        "Wed Nov 15 03:43:20 2023 +0530",
        "2023-11-15 03:43:20 +0530"
    )]
    #[case::west(
        1_696_320_000,
        -480,
        "Tue Oct 3 00:00:00 2023 -0800",
        "2023-10-03 00:00:00 -0800"
    )]
    #[case::leap_day(
        1_709_210_096,
        840,
        "Fri Mar 1 02:34:56 2024 +1400",
        "2024-03-01 02:34:56 +1400"
    )]
    #[case::large_offset(
        1_700_000_000,
        5999,
        "Sun Nov 19 02:12:20 2023 +9959",
        "2023-11-19 02:12:20 +9959"
    )]
    #[case::year_9999(
        253_402_300_799,
        0,
        "Fri Dec 31 23:59:59 9999 +0000",
        "9999-12-31 23:59:59 +0000"
    )]
    #[case::year_5138(
        99_999_999_999,
        0,
        "Wed Nov 16 09:46:39 5138 +0000",
        "5138-11-16 09:46:39 +0000"
    )]
    fn formats_dates(
        #[case] seconds: i64,
        #[case] offset_minutes: i16,
        #[case] default: &str,
        #[case] iso: &str,
    ) {
        let date = IdentityDate {
            seconds,
            offset_minutes,
        };

        assert_eq!(date.format(DateFormat::Default).to_string(), default);
        assert_eq!(date.format(DateFormat::Iso).to_string(), iso);
    }

    /// Git refuses pre-epoch local times; girt still renders the exact proleptic date.
    #[test]
    fn formats_pre_epoch_dates() {
        let date = IdentityDate {
            seconds: -86_400,
            offset_minutes: 0,
        };

        assert_eq!(
            date.format(DateFormat::Iso).to_string(),
            "1969-12-31 00:00:00 +0000"
        );
    }

    /// Extreme instants and offsets do not overflow.
    #[test]
    fn formats_extreme_dates() {
        let date = IdentityDate {
            seconds: i64::MAX,
            offset_minutes: i16::MAX,
        };

        assert!(
            date.format(DateFormat::Default)
                .to_string()
                .ends_with(" +54607")
        );
    }
}
