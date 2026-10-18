//! `transfer.hideRefs` and per-service `hideRefs` rules shared by serving and local push.

use crate::Repository;
use crate::refs::RefName;

/// Ordered hide patterns from `transfer.hideRefs` and one service section's `hideRefs`.
///
/// A later matching pattern wins; a `!` prefix makes matching names visible again.
#[derive(Debug, Clone, Default)]
pub(crate) struct HideRules(Vec<(Vec<u8>, bool)>);

impl HideRules {
    /// Reads the rules for `service`, the `uploadpack` or `receive` config section.
    ///
    /// Returns the unsupported spelling when a value is implicit or empty.
    pub(crate) fn parse(repository: &Repository, service: &str) -> Result<Self, &'static str> {
        let mut rules = Vec::new();
        for entry in repository.config().entries() {
            if entry.subsection.is_some()
                || !entry.name.eq_ignore_ascii_case(b"hiderefs")
                || !(entry.section.eq_ignore_ascii_case(b"transfer")
                    || entry.section.eq_ignore_ascii_case(service.as_bytes()))
            {
                continue;
            }
            let value = entry.value.as_deref().ok_or("implicit hideRefs pattern")?;
            let (visible, pattern) = if let Some(pattern) = value.strip_prefix(b"!") {
                (true, pattern)
            } else {
                (false, value)
            };
            // Namespace rewriting is not supported, so ^ matches the same spelling as an ordinary
            // pattern. Reject an empty pattern rather than hiding all refs.
            let pattern = pattern.strip_prefix(b"^").unwrap_or(pattern);
            if pattern.is_empty() {
                return Err("empty hideRefs pattern");
            }
            rules.push((pattern.to_vec(), visible));
        }
        Ok(Self(rules))
    }

    /// Whether `name` is hidden.
    pub(crate) fn matches(&self, name: &RefName) -> bool {
        let mut hidden = false;
        for (pattern, visible) in &self.0 {
            let pattern = pattern.strip_suffix(b"/").unwrap_or(pattern);
            let rest = name.as_bytes().strip_prefix(pattern);
            if rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/")) {
                hidden = !visible;
            }
        }
        hidden
    }
}
