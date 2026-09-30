use std::ops::Range;

use super::Entry;
use super::parse::SectionOccurrence;

/// Placement of successfully resolved include contents in the effective snapshot.
///
/// Both modes validate sources in forward, depth-first directive order and retain the same source
/// provenance and resource limits. This selects output placement, not parsing or trust policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IncludePlacement {
    /// Expand each include at its directive, preserving Git's ordinary configuration order.
    #[default]
    InPlace,
    /// Emit every physical parent section in full, then its included child blocks in reverse
    /// directive order. Apply the same placement recursively within each child. Separate parent
    /// sections retain their order, including empty sections. Members of an already-resolved input
    /// section are gathered together before its newly included children.
    AfterSectionReverse,
}

/// Records only output section ranges. Resolution, validation and accounting stay in the resolver.
pub(super) struct SectionPlacement {
    first_entry: usize,
    first_section: usize,
    children: Vec<Vec<Range<usize>>>,
}

impl SectionPlacement {
    pub(super) fn new(entries: usize, sections: usize, parent_sections: usize) -> Self {
        Self {
            first_entry: entries,
            first_section: sections,
            children: vec![Vec::new(); parent_sections],
        }
    }

    pub(super) fn included(&mut self, owner: usize, sections: Range<usize>) {
        if !sections.is_empty() {
            self.children[owner].push(sections);
        }
    }

    pub(super) fn apply(
        self,
        parents: Vec<usize>,
        entries: &mut Vec<Entry>,
        sections: &mut Vec<SectionOccurrence>,
    ) {
        // Every entry has exactly one physical owner. Move values rather than cloning them;
        // temporary slots and ranges are bounded by the existing entry/section budgets.
        let mut old_entries: Vec<_> = entries.drain(self.first_entry..).map(Some).collect();
        let mut old_sections: Vec<_> = sections.drain(self.first_section..).map(Some).collect();
        let order = parents
            .into_iter()
            .zip(self.children)
            .flat_map(|(parent, children)| {
                std::iter::once(parent).chain(children.into_iter().rev().flatten())
            });
        for index in order {
            let mut section = old_sections[index - self.first_section]
                .take()
                .expect("each resolved section has one placement");
            section.start = entries.len();
            for member in &mut section.entries {
                let entry = old_entries[*member - self.first_entry]
                    .take()
                    .expect("each resolved entry has one physical owner");
                *member = entries.len();
                entries.push(entry);
            }
            sections.push(section);
        }
        debug_assert!(old_entries.iter().all(Option::is_none));
        debug_assert!(old_sections.iter().all(Option::is_none));
    }
}

#[cfg(test)]
mod tests;
