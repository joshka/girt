//! Independent edit-graph implementation informed by Myers (1986), section 3.
//! No upstream implementation or fixtures were used. See docs/compatibility.md for provenance.
use super::{Budget, ContentEdit, DiffError, Lines, charge, equal};

pub(super) fn edits(
    old: &Lines<'_>,
    new: &Lines<'_>,
    mut trace_left: usize,
    budget: &mut Budget<'_>,
) -> Result<Vec<ContentEdit>, DiffError> {
    if old.len() == 0 || new.len() == 0 {
        budget.work(1)?;
        return Ok(vec![edit(old, new, 0..old.len(), 0..new.len())]);
    }
    let total = old
        .len()
        .checked_add(new.len())
        .ok_or(DiffError::Limit("lines"))?;
    let mut trace: Vec<Vec<usize>> = Vec::new();
    for distance in 0..=total {
        let width = distance.checked_add(1).ok_or(DiffError::Limit("trace"))?;
        charge(&mut trace_left, width, "trace")?;
        budget.check()?;
        let mut row = Vec::with_capacity(width);
        // Slot i is diagonal k = 2*i - distance. Store only reachable parity positions.
        for slot in 0..width {
            budget.work(1)?;
            let mut x = if distance == 0 {
                0
            } else {
                let previous = &trace[distance - 1];
                if deletion(previous, slot) {
                    previous[slot - 1]
                        .checked_add(1)
                        .ok_or(DiffError::Limit("lines"))?
                } else {
                    previous[slot]
                }
            };
            let mut y = new_position(x, distance, slot)?;
            while x < old.len() && y < new.len() {
                budget.work(1)?;
                if !equal(old.line(x), new.line(y), budget)? {
                    break;
                }
                x += 1;
                y += 1;
            }
            row.push(x);
            if x == old.len() && y == new.len() {
                // The final row is unnecessary for traceback; only its predecessor is used.
                return backtrack(old, new, &trace, slot, budget);
            }
        }
        trace.push(row);
    }
    unreachable!("the all-delete/all-insert path reaches the destination")
}

/// Equal predecessor x values select deletion. Reuse this rule during traceback.
fn deletion(previous: &[usize], slot: usize) -> bool {
    slot != 0 && (slot == previous.len() || previous[slot - 1] >= previous[slot])
}

fn backtrack(
    old: &Lines<'_>,
    new: &Lines<'_>,
    trace: &[Vec<usize>],
    mut slot: usize,
    budget: &mut Budget<'_>,
) -> Result<Vec<ContentEdit>, DiffError> {
    let (mut x, mut y) = (old.len(), new.len());
    let mut edits: Vec<ContentEdit> = Vec::new();
    for distance in (1..=trace.len()).rev() {
        budget.work(1)?;
        let previous = &trace[distance - 1];
        let removed = deletion(previous, slot);
        let previous_slot = slot - usize::from(removed);
        let previous_x = previous[previous_slot];
        let previous_y = new_position(previous_x, distance - 1, previous_slot)?;
        let end_x = previous_x + usize::from(removed);
        let end_y = previous_y + usize::from(!removed);
        // Skip the matched diagonal in constant time. Consecutive edit edges form one span.
        debug_assert_eq!(x - end_x, y - end_y);
        if let Some(last) = edits
            .last_mut()
            .filter(|last| last.old_lines.start == end_x && last.new_lines.start == end_y)
        {
            last.old_lines.start = previous_x;
            last.new_lines.start = previous_y;
            last.old_bytes.start = old.offset(previous_x);
            last.new_bytes.start = new.offset(previous_y);
        } else {
            edits.push(edit(old, new, previous_x..end_x, previous_y..end_y));
        }
        x = previous_x;
        y = previous_y;
        slot = previous_slot;
    }
    edits.reverse();
    budget.check()?;
    Ok(edits)
}

fn edit(
    old: &Lines<'_>,
    new: &Lines<'_>,
    old_lines: std::ops::Range<usize>,
    new_lines: std::ops::Range<usize>,
) -> ContentEdit {
    ContentEdit {
        old_bytes: old.range(&old_lines),
        new_bytes: new.range(&new_lines),
        old_lines,
        new_lines,
    }
}

/// Convert a compact frontier slot to y without signed coordinates or doubling the slot.
fn new_position(x: usize, distance: usize, slot: usize) -> Result<usize, DiffError> {
    let opposite = distance - slot;
    let y = if slot <= opposite {
        x.checked_add(opposite - slot)
    } else {
        x.checked_sub(slot - opposite)
    };
    y.ok_or(DiffError::Limit("lines"))
}
