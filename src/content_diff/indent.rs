//! Indent-aware boundary selection calibrated against original Git CLI fixtures.
//!
//! This deliberately small scoring rule favours lower indentation and blank separators. It
//! reproduces common Git placements, but is not an exact model of every Git scoring tie.
use super::{Budget, DiffError, Lines};

pub(super) fn boundary(
    lines: &Lines<'_>,
    earliest: usize,
    last: usize,
    length: usize,
    budget: &mut Budget<'_>,
) -> Result<usize, DiffError> {
    if earliest == last {
        return Ok(last);
    }
    // The CLI's candidate window grows with the group, up to 100 rotations. Scan forward:
    // the comparison is non-transitive, and equal scores select the later boundary.
    let first = earliest.max(last.saturating_sub(length.saturating_add(1).min(100)));
    let mut best = first;
    let mut best_score = group(lines, first, length, budget)?;
    for position in first + 1..=last {
        budget.work(1)?;
        let score = group(lines, position, length, budget)?;
        if score.preferred_to(best_score) {
            best = position;
            best_score = score;
        }
    }
    Ok(best)
}

#[derive(Clone, Copy)]
struct Score {
    indent: i64,
    penalty: i64,
}

impl Score {
    fn preferred_to(self, other: Self) -> bool {
        7 * (self.indent - other.indent).signum() + self.penalty - other.penalty <= 0
    }
}

fn group(
    lines: &Lines<'_>,
    start: usize,
    length: usize,
    budget: &mut Budget<'_>,
) -> Result<Score, DiffError> {
    let before = split(lines, start, budget)?;
    let after = split(lines, start + length, budget)?;
    Ok(Score {
        indent: before.indent + after.indent,
        penalty: before.penalty + after.penalty,
    })
}

fn split(lines: &Lines<'_>, position: usize, budget: &mut Budget<'_>) -> Result<Score, DiffError> {
    let (before, above) = neighbour(lines, position, true, budget)?;
    let (after, below) = neighbour(lines, position, false, budget)?;
    let relation = match (before, after) {
        (_, None) => 0,
        (None, _) => 1,
        (Some(before), Some(after)) => match after.cmp(&before) {
            std::cmp::Ordering::Greater => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Less => 3,
        },
    };
    Ok(Score {
        indent: after.unwrap_or(-1),
        penalty: relation - 3 * above.min(9) - 3 * below.min(2) + i64::from(below > 0),
    })
}

/// Bound context inspection independently of the file's blank-run length. The score saturates
/// before this cap; extra context only identifies the adjacent nonblank indentation.
fn neighbour(
    lines: &Lines<'_>,
    mut position: usize,
    backwards: bool,
    budget: &mut Budget<'_>,
) -> Result<(Option<i64>, i64), DiffError> {
    let mut blanks = 0;
    while blanks < 20 {
        budget.work(1)?;
        let index = if backwards {
            let Some(index) = position.checked_sub(1) else {
                return Ok((None, blanks));
            };
            position = index;
            index
        } else {
            if position == lines.len() {
                return Ok((None, blanks));
            }
            let index = position;
            position += 1;
            index
        };
        if let Some(indent) = indentation(lines.line(index), budget)? {
            return Ok((Some(indent), blanks));
        }
        blanks += 1;
    }
    Ok((None, blanks))
}

fn indentation(line: &[u8], budget: &mut Budget<'_>) -> Result<Option<i64>, DiffError> {
    let mut column = 0i64;
    for &byte in line {
        budget.work(1)?;
        match byte {
            b' ' => column = column.saturating_add(1),
            b'\t' => column = column.saturating_add(8 - column % 8),
            b'\r' | b'\n' | 0x0b | 0x0c => {}
            _ => return Ok(Some(column)),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn repeated_lines() -> Lines<'static> {
        Lines {
            bytes: b"a\na\na\n",
            ends: vec![2, 4, 6],
        }
    }

    #[test]
    fn scoring_exhausts_the_work_budget() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: 1,
            cancel: &cancel,
        };
        let result = boundary(&repeated_lines(), 0, 2, 1, &mut budget);
        assert!(matches!(result, Err(DiffError::Limit("work"))));
    }

    #[test]
    fn scoring_observes_cancellation_after_prior_candidate_work() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: usize::MAX,
            cancel: &cancel,
        };
        let first = group(&repeated_lines(), 0, 1, &mut budget).unwrap();
        assert_eq!(first.indent, 0);
        cancel.store(true, Ordering::Relaxed);
        let result = boundary(&repeated_lines(), 0, 2, 1, &mut budget);
        assert!(matches!(result, Err(DiffError::Cancelled)));
    }
}
