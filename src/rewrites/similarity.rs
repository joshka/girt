use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Limits};

/// One operation's shared accounting, including exact matching and all edited candidates.
pub(super) struct Budget<'a> {
    pub remaining: Limits,
    pub cancel: &'a AtomicBool,
}

impl Budget<'_> {
    pub fn check(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn work(&mut self, amount: usize) -> Result<(), Error> {
        self.check()?;
        spend(&mut self.remaining.max_work, amount, "work")
    }

    pub fn pair(&mut self) -> Result<(), Error> {
        self.work(1)?;
        spend(&mut self.remaining.max_pairs, 1, "pairs")
    }
}

pub(super) fn spend(remaining: &mut usize, amount: usize, name: &'static str) -> Result<(), Error> {
    *remaining = remaining.checked_sub(amount).ok_or(Error::Limit(name))?;
    Ok(())
}

/// Distinct bounded byte spans with aggregate retained-byte weights.
pub(super) struct Content {
    pub length: usize,
    pub spans: BTreeMap<Vec<u8>, usize>,
}

impl Content {
    pub fn new(bytes: &[u8], budget: &mut Budget<'_>) -> Result<Self, Error> {
        budget.work(bytes.len().min(8000))?;
        let binary = bytes[..bytes.len().min(8000)].contains(&0);
        let mut spans = BTreeMap::new();
        let mut span = Vec::with_capacity(64);
        for (index, &byte) in bytes.iter().enumerate() {
            budget.work(1)?;
            if !binary && byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                continue;
            }
            span.push(byte);
            if byte == b'\n' || span.len() == 64 {
                Self::insert(&mut spans, &mut span, budget)?;
            }
        }
        if !span.is_empty() {
            Self::insert(&mut spans, &mut span, budget)?;
        }
        Ok(Self {
            length: bytes.len(),
            spans,
        })
    }

    fn insert(
        spans: &mut BTreeMap<Vec<u8>, usize>,
        span: &mut Vec<u8>,
        budget: &mut Budget<'_>,
    ) -> Result<(), Error> {
        spend(&mut budget.remaining.max_spans, 1, "spans")?;
        // A key comparison examines at most 64 bytes, over logarithmically many tree levels.
        budget.work(64 * (spans.len() + 1).ilog2() as usize + 64)?;
        *spans.entry(span.clone()).or_default() += span.len();
        span.clear();
        Ok(())
    }

    pub fn score(
        &self,
        other: &Self,
        minimum: u8,
        budget: &mut Budget<'_>,
    ) -> Result<Option<u8>, Error> {
        let total = self.length.max(other.length) as u128;
        if total == 0 || self.length.min(other.length) as u128 * 100 < total * u128::from(minimum) {
            return Ok(None);
        }
        let mut retained = 0u128;
        for (span, count) in &self.spans {
            budget.work(64 * (other.spans.len() + 1).ilog2() as usize + 64)?;
            if let Some(other_count) = other.spans.get(span) {
                retained += (*count).min(*other_count) as u128;
            }
        }
        Ok((retained * 100 >= total * u128::from(minimum))
            .then_some((retained * 100 / total) as u8))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::half(b"a\nb\nc\nd\n", b"a\nb\nx\ny\n", Some(50))]
    #[case::reordered(b"a\nb\nc\nd\n", b"d\nc\nb\na\n", Some(100))]
    #[case::fragment(b"a\nb", b"a\nc", Some(66))]
    #[case::crlf(b"a\r\nb\r\n", b"a\nb\n", Some(66))]
    #[case::binary_crlf(b"\0\r\nb\r\n", b"\0\nb\n", None)]
    #[case::binary_lines(b"\0\na\nb\nc\n", b"\0\na\nb\nx\n", Some(75))]
    #[case::multiplicity(b"a\na\nb\nb\n", b"a\nb\nc\nc\n", Some(50))]
    #[case::below(b"a\nb\n", b"a\nb\nc\nd\ne\n", None)]
    fn retained_bytes(#[case] old: &[u8], #[case] new: &[u8], #[case] expected: Option<u8>) {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: Limits::default(),
            cancel: &cancel,
        };
        let old = Content::new(old, &mut budget).unwrap();
        let new = Content::new(new, &mut budget).unwrap();
        assert_eq!(old.score(&new, 50, &mut budget).unwrap(), expected);
    }

    #[test]
    fn cancellation_precedes_work_exhaustion() {
        let cancel = AtomicBool::new(true);
        let mut budget = Budget {
            remaining: Limits {
                max_work: 0,
                ..Limits::default()
            },
            cancel: &cancel,
        };
        assert!(matches!(
            Content::new(b"a", &mut budget),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn span_limit_counts_repeated_occurrences() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: Limits {
                max_spans: 1,
                ..Limits::default()
            },
            cancel: &cancel,
        };
        assert!(matches!(
            Content::new(b"a\na\n", &mut budget),
            Err(Error::Limit("spans"))
        ));
    }
}
