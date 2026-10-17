//! Explicit, owned interpretation of Git reflog expiry configuration.
use crate::Config;
use crate::config::{InvalidValue, matches_without_pathname};
use crate::refs::RefName;

/// Resolved timestamp cutoffs for Git's two independent expiry rules.
///
/// `None` disables a rule. A disconnected record can qualify under either cutoff; the general
/// cutoff also applies to live records. Timestamps are Unix seconds and comparison is strict.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ReflogCutoffs {
    /// Expire any record with a timestamp strictly before this value.
    pub general: Option<i128>,

    /// Also expire disconnected records strictly before this value.
    pub unreachable: Option<i128>,
}

impl ReflogCutoffs {
    /// Sets [`Self::general`].
    #[must_use]
    pub fn with_general(mut self, general: Option<i128>) -> Self {
        self.general = general;
        self
    }

    /// Sets [`Self::unreachable`].
    #[must_use]
    pub fn with_unreachable(mut self, unreachable: Option<i128>) -> Self {
        self.unreachable = unreachable;
        self
    }
}

/// Git reflog expiry settings resolved against one caller-supplied Unix clock.
///
/// Opt into this policy through [`super::ReflogExpiryPolicy::Configured`]. Global
/// defaults expire all entries after 90 days and disconnected entries after 30 days. The first
/// matching `gc.<pattern>` supplies both rules, with unset keys disabling expiry and the last value
/// for each key in that pattern winning. Patterns match reference bytes case-sensitively; `*` can
/// cross `/`. Without a matching pattern, `refs/stash` keeps every record, even when global
/// settings request expiry.
///
/// Supports `never`, `false`, `now`, `all`, decimal Unix timestamps in `100,000,000..=i64::MAX`,
/// `0` for the supplied clock, and numeric second/minute/hour/day/week intervals with spaces or
/// dots and optional `ago`. Other Git date expressions are explicitly unsupported. `never` returns
/// no cutoff; exact lowercase `now` and `all` return Git's maximum unsigned timestamp. Case
/// variants or quoted surrounding whitespace for `now` resolve to the supplied clock. No
/// environment, clock or storage is read here.
///
/// ```
/// use girt::Config;
/// use girt::refs::RefName;
/// use girt::retention::ReflogExpiry;
/// let config = Config::parse(b"[gc]\nreflogExpire=never\n")?;
/// let expiry = ReflogExpiry::from_config(&config, 1_800_000_000)?;
/// let cutoffs = expiry.cutoffs(&RefName::new(b"refs/heads/main")?)?;
/// assert_eq!(cutoffs.general, None);
/// assert_eq!(cutoffs.unreachable, Some(1_800_000_000 - 30 * 86400));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct ReflogExpiry {
    defaults: ReflogCutoffs,
    patterns: Vec<Pattern>,
}

#[derive(Clone, Debug)]
struct Pattern {
    bytes: Vec<u8>,
    cutoffs: ReflogCutoffs,
}

/// Reflog configuration cannot safely select an expiry policy.
///
/// An unsupported date expression is a refusal, not a request to fall back to immediate expiry.
/// Supply a supported explicit policy or fix configuration before trying again.
///
/// ```
/// use girt::retention::{ReflogExpiry, ReflogExpiryError};
/// let config = girt::Config::parse(b"[gc]\nreflogExpire=yesterday\n")?;
/// assert!(matches!(
///     ReflogExpiry::from_config(&config, 1_800_000_000),
///     Err(ReflogExpiryError::Value(_))
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReflogExpiryError {
    /// Malformed or unsupported date expression, including an implicit boolean.
    #[error("invalid or unsupported reflog expiry configuration")]
    Value(#[source] InvalidValue),
    /// The supplied Unix clock is outside Git's unsigned timestamp range.
    #[error("reflog expiry clock is outside Git's timestamp range")]
    Clock,
    /// A pattern exceeds the one-million-cell matching bound.
    #[error("reflog expiry pattern work limit exceeded")]
    PatternLimit,
}

impl ReflogExpiry {
    /// Resolves every expiry occurrence in the supplied configuration snapshot.
    ///
    /// `now` is Unix seconds captured by the caller, in `0..=u64::MAX`. Relative intervals that
    /// reach before the epoch are unsupported. Later duplicate keys replace earlier values;
    /// malformed earlier occurrences still refuse construction, as Git does.
    ///
    /// # Errors
    ///
    /// Returns [`ReflogExpiryError`] for an invalid clock, unsupported or invalid date, or a
    /// pattern too large to match any valid reference within the work bound. No files are
    /// changed.
    ///
    /// ```
    /// use girt::refs::RefName;
    /// use girt::retention::ReflogExpiry;
    /// let config =
    ///     girt::Config::parse(b"[gc]\nreflogExpire=never\nreflogExpireUnreachable=7 days ago\n")?;
    /// let expiry = ReflogExpiry::from_config(&config, 1_800_000_000)?;
    /// let cutoffs = expiry.cutoffs(&RefName::new("refs/heads/main")?)?;
    /// assert_eq!(cutoffs.general, None);
    /// assert_eq!(cutoffs.unreachable, Some(1_800_000_000 - 7 * 86400));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_config(config: &Config, now: i128) -> Result<Self, ReflogExpiryError> {
        if !(0..=u64::MAX as i128).contains(&now) {
            return Err(ReflogExpiryError::Clock);
        }
        let mut result = Self {
            defaults: ReflogCutoffs {
                general: cutoff(now - 90 * 86400),
                unreachable: cutoff(now - 30 * 86400),
            },
            patterns: Vec::new(),
        };
        for entry in config.entries() {
            if !entry.section.eq_ignore_ascii_case(b"gc") {
                continue;
            }
            let general = entry.name.eq_ignore_ascii_case(b"reflogexpire");
            if !general && !entry.name.eq_ignore_ascii_case(b"reflogexpireunreachable") {
                continue;
            }
            let value = entry
                .value
                .as_deref()
                .and_then(|value| parse_date(value, now));
            let value = value.ok_or_else(|| {
                ReflogExpiryError::Value(InvalidValue::new(
                    "gc",
                    if general {
                        "reflogexpire"
                    } else {
                        "reflogexpireunreachable"
                    },
                    entry.value.as_deref(),
                ))
            })?;
            let Some(bytes) = &entry.subsection else {
                if general {
                    result.defaults.general = value;
                } else {
                    result.defaults.unreachable = value;
                }
                continue;
            };
            if bytes.len().saturating_add(1).saturating_mul(2) > 1_000_000 {
                return Err(ReflogExpiryError::PatternLimit);
            }
            let index = match result
                .patterns
                .iter()
                .position(|pattern| pattern.bytes == *bytes)
            {
                Some(index) => index,
                None => {
                    result.patterns.push(Pattern {
                        bytes: bytes.clone(),
                        cutoffs: ReflogCutoffs::default(),
                    });
                    result.patterns.len() - 1
                }
            };
            if general {
                result.patterns[index].cutoffs.general = value;
            } else {
                result.patterns[index].cutoffs.unreachable = value;
            }
        }
        Ok(result)
    }

    /// Returns the general and disconnected-record cutoffs for this history.
    ///
    /// A record expires when its timestamp is strictly below the general cutoff, or when it is
    /// disconnected and strictly below the second cutoff. `None` disables that expiry rule.
    ///
    /// # Errors
    ///
    /// Returns [`ReflogExpiryError::PatternLimit`] when a pattern/reference pair exceeds one
    /// million matching cells. Retention planning reports incomplete before any mutation on
    /// this error.
    pub fn cutoffs(&self, name: &RefName) -> Result<ReflogCutoffs, ReflogExpiryError> {
        for pattern in &self.patterns {
            if matches_without_pathname(&pattern.bytes, name.as_bytes(), 1_000_000)
                .ok_or(ReflogExpiryError::PatternLimit)?
            {
                return Ok(pattern.cutoffs);
            }
        }
        if name.as_bytes() == b"refs/stash" {
            Ok(ReflogCutoffs::default())
        } else {
            Ok(self.defaults)
        }
    }
}

fn cutoff(seconds: i128) -> Option<i128> {
    (seconds > 0).then_some(seconds)
}

fn parse_date(value: &[u8], now: i128) -> Option<Option<i128>> {
    let raw = std::str::from_utf8(value).ok()?;
    let text = raw.trim().to_ascii_lowercase();
    // Git reserves the maximum-timestamp sentinel for exact lowercase keywords; other `now`
    // spellings follow its approximate-date clock. `false` is accepted only exactly as written.
    match (raw, text.as_str()) {
        ("now" | "all", _) => return Some(Some(u64::MAX as i128)),
        ("false", _) | (_, "never") => return Some(None),
        (_, "now") => return Some(cutoff(now)),
        _ => {}
    }
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        let timestamp = text.parse::<u64>().ok()?;
        // Git interprets short numbers through its approximate-date grammar; don't guess.
        if timestamp == 0 {
            return Some(cutoff(now));
        }
        return (100_000_000..=i64::MAX as u64)
            .contains(&timestamp)
            .then_some(Some(timestamp as i128));
    }
    let mut words = text.split(['.', ' ']).filter(|word| !word.is_empty());
    let count = words.next()?.parse::<u64>().ok()?;
    let unit = words.next()?;
    let seconds = match unit.strip_suffix('s').unwrap_or(unit) {
        "second" => 1,
        "minute" => 60,
        "hour" => 3600,
        "day" => 86400,
        "week" => 7 * 86400,
        _ => return None,
    };
    match (words.next(), words.next()) {
        (None, None) | (Some("ago"), None) => {}
        _ => return None,
    }
    let age = count.checked_mul(seconds)?;
    if age as i128 > now {
        return None;
    }
    Some(cutoff(now - age as i128))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::week(b"2.weeks.ago", Some(Some(1_800_000_000 - 14 * 86400)))]
    #[case::epoch(b"18446744073709551615 weeks", None)]
    #[case::before_epoch(b"3000 weeks", None)]
    #[case::unsigned_overflow(b"18446744073709551616", None)]
    #[case::implicit_boolean(b"true", None)]
    #[case::unsupported_calendar(b"1.month.ago", None)]
    #[case::unsupported_absolute(b"2020-01-02", None)]
    #[case::unsupported_natural(b"yesterday", None)]
    fn dates(#[case] bytes: &[u8], #[case] expected: Option<Option<i128>>) {
        assert_eq!(parse_date(bytes, 1_800_000_000), expected);
    }

    #[test]
    fn invalid_earlier_value_is_not_hidden_by_later_duplicate() {
        let config = Config::parse(b"[gc]\nreflogExpire=invalid\nreflogExpire=never\n").unwrap();
        assert!(matches!(
            ReflogExpiry::from_config(&config, 1_800_000_000),
            Err(ReflogExpiryError::Value(_))
        ));
    }

    #[test]
    fn oversized_match_refuses_before_planning_can_authorize_expiry() {
        let bytes = [
            b"[gc \"".as_slice(),
            &vec![b'a'; 2000],
            b"\"]\nreflogExpire=now\n",
        ]
        .concat();
        let expiry =
            ReflogExpiry::from_config(&Config::parse(&bytes).unwrap(), 1_800_000_000).unwrap();
        let name = RefName::new([b"refs/heads/".as_slice(), &vec![b'a'; 1000]].concat()).unwrap();
        assert!(matches!(
            expiry.cutoffs(&name),
            Err(ReflogExpiryError::PatternLimit)
        ));
    }
}
