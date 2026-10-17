//! Boundary rotations inferred from original fixtures through the Git CLI only.
//!
//! A rotation exchanges an edge of a changed group with an identical neighbouring line. The
//! unchanged subsequence and changed-line count stay fixed, so shortest edits remain shortest.
use super::{Budget, ContentEdit, DiffError, Lines, equal};

pub(super) fn edits(
    old: &Lines<'_>,
    new: &Lines<'_>,
    edits: Vec<ContentEdit>,
    indent_heuristic: bool,
    budget: &mut Budget<'_>,
) -> Result<Vec<ContentEdit>, DiffError> {
    // With no retained lines there is no equivalent boundary to choose. This also preserves
    // the constant-space empty-side path and its work budget.
    if edits.len() == 1
        && edits[0].old_lines == (0..old.len())
        && edits[0].new_lines == (0..new.len())
    {
        return Ok(edits);
    }
    let mut removed = changed(old.len(), &edits, true, budget)?;
    let mut added = changed(new.len(), &edits, false, budget)?;
    drop(edits);
    compact(old, &mut removed, &added, indent_heuristic, budget)?;
    compact(new, &mut added, &removed, indent_heuristic, budget)?;
    collect(old, new, &removed, &added, budget)
}

fn changed(
    count: usize,
    edits: &[ContentEdit],
    old: bool,
    budget: &mut Budget<'_>,
) -> Result<Vec<bool>, DiffError> {
    budget.work(count)?;
    let mut changed = vec![false; count];
    for edit in edits {
        budget.work(1)?;
        let range = if old {
            &edit.old_lines
        } else {
            &edit.new_lines
        };
        budget.work(range.len())?;
        changed[range.clone()].fill(true);
    }
    Ok(changed)
}

/// The rank of a boundary counts retained lines, so it is shared by both inputs even when
/// their changed groups have different lengths. Matching ranks combine a deletion and addition.
fn changed_boundaries(changed: &[bool], budget: &mut Budget<'_>) -> Result<Vec<bool>, DiffError> {
    let mut boundaries = vec![false];
    for &changed in changed {
        budget.work(1)?;
        if changed {
            *boundaries.last_mut().expect("the initial boundary exists") = true;
        } else {
            boundaries.push(false);
        }
    }
    Ok(boundaries)
}

fn compact(
    lines: &Lines<'_>,
    changed: &mut [bool],
    opposite: &[bool],
    indent_heuristic: bool,
    budget: &mut Budget<'_>,
) -> Result<(), DiffError> {
    let boundaries = changed_boundaries(opposite, budget)?;
    let (mut cursor, mut rank) = (0, 0);
    while cursor < changed.len() {
        budget.work(1)?;
        if !changed[cursor] {
            cursor += 1;
            rank += 1;
            continue;
        }
        let mut start = cursor;
        let mut end = group_end(changed, start, budget)?;
        let (aligned, earliest) = loop {
            let length = end - start;
            while start > 0 && equal(lines.line(start - 1), lines.line(end - 1), budget)? {
                rotate_up(changed, &mut start, &mut end);
                rank -= 1;
                while start > 0 && changed[start - 1] {
                    budget.work(1)?;
                    start -= 1;
                }
            }
            let earliest = start;
            let mut aligned = None;
            loop {
                budget.work(1)?;
                if boundaries[rank] {
                    aligned = Some(start);
                }
                if end == changed.len() || !equal(lines.line(start), lines.line(end), budget)? {
                    break;
                }
                changed[start] = false;
                changed[end] = true;
                start += 1;
                rank += 1;
                end = group_end(changed, end, budget)?;
            }
            // A rotation can meet another group. Explore the combined group again before
            // choosing a boundary; merging takes precedence over alignment with the other side.
            if end - start == length {
                break (aligned, earliest);
            }
        };
        let destination = if aligned.is_some() || !indent_heuristic {
            aligned
        } else {
            Some(super::indent::boundary(
                lines,
                earliest,
                start,
                end - start,
                budget,
            )?)
        };
        if let Some(destination) = destination {
            while start > destination {
                budget.work(1)?;
                rotate_up(changed, &mut start, &mut end);
                rank -= 1;
            }
        }
        cursor = end;
    }
    Ok(())
}

fn group_end(
    changed: &[bool],
    mut end: usize,
    budget: &mut Budget<'_>,
) -> Result<usize, DiffError> {
    while end < changed.len() && changed[end] {
        budget.work(1)?;
        end += 1;
    }
    Ok(end)
}

fn rotate_up(changed: &mut [bool], start: &mut usize, end: &mut usize) {
    changed[*end - 1] = false;
    *start -= 1;
    changed[*start] = true;
    *end -= 1;
}

fn collect(
    old: &Lines<'_>,
    new: &Lines<'_>,
    removed: &[bool],
    added: &[bool],
    budget: &mut Budget<'_>,
) -> Result<Vec<ContentEdit>, DiffError> {
    let (mut x, mut y) = (0, 0);
    let mut edits = Vec::new();
    while x < removed.len() || y < added.len() {
        budget.work(1)?;
        let end_x = group_end(removed, x, budget)?;
        let end_y = group_end(added, y, budget)?;
        if end_x != x || end_y != y {
            let old_lines = x..end_x;
            let new_lines = y..end_y;
            edits.push(ContentEdit {
                old_bytes: old.range(&old_lines),
                new_bytes: new.range(&new_lines),
                old_lines,
                new_lines,
            });
            x = end_x;
            y = end_y;
        } else {
            x += 1;
            y += 1;
        }
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    // A middle insertion can rotate across both identical neighbours.
    fn repeated_lines() -> Lines<'static> {
        Lines {
            bytes: b"a\na\na\n",
            ends: vec![2, 4, 6],
        }
    }

    #[rstest::rstest]
    fn rotations_exhaust_the_work_budget(#[values(false, true)] indent: bool) {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: 7,
            cancel: &cancel,
        };
        let mut changed = [false, true, false];
        let result = compact(
            &repeated_lines(),
            &mut changed,
            &[false, false],
            indent,
            &mut budget,
        );
        assert!(matches!(result, Err(DiffError::Limit("work"))));
        assert_eq!(changed, [true, false, false]);
    }

    #[rstest::rstest]
    fn cancellation_between_compaction_passes_stops_work(#[values(false, true)] indent: bool) {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget {
            remaining: usize::MAX,
            cancel: &cancel,
        };
        let mut changed = [false, true, false];
        compact(
            &repeated_lines(),
            &mut changed,
            &[false, false],
            indent,
            &mut budget,
        )
        .unwrap();
        assert_eq!(changed, [false, false, true]);
        cancel.store(true, Ordering::Relaxed);
        let result = compact(
            &repeated_lines(),
            &mut changed,
            &[false, false],
            indent,
            &mut budget,
        );
        assert!(matches!(result, Err(DiffError::Cancelled)));
        assert_eq!(changed, [false, false, true]);
    }
}
