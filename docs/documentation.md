# Documentation Standard

Documentation must help readers understand behavior and make decisions without reconstructing
missing context. Requirements protect accuracy; writing preferences guide judgment rather than
prescribe a template.

## Requirements

- Describe implemented behavior; distinguish goals, verified facts, and unresolved inference.
- Verify changed claims against code and tests. Investigate history when needed to establish
  rationale; never invent evidence to make prose more concrete.
- Update affected documentation with behavior changes. Keep corrections scoped to the subject.
- Preserve reasons, conditions, qualifications, examples, and established Git terminology.
- Explain enough locally that external sources provide optional depth. Link to canonical
  explanations and verify changed commands, paths, and destinations.

## Writing Preferences

- Lead with the useful point and name the actor, operation, outcome, or tradeoff.
- Replace vague polish and inflated claims with the behavior that earns them. Question invented
  labels, formulaic contrasts, page narration, and repetitive summaries when they add no
  explanation.
- Keep girt's direct tone and vocabulary. Use connected paragraphs for relationships and short lists
  for independent instructions, steps, or alternatives. Headings should identify substantial topics.
- Repair weak sentences before deleting them. Stop when edits only exchange equivalent wording.

These preferences are revision signals, not forbidden words or proof of authorship. After editing,
readers must still understand why the behavior exists, when it applies, and what can go wrong.
Concision must not produce choppy fragments that force readers to supply the connections.

Illustrative revisions grounded in girt's current behavior:

- Before: "girt supports all Git repositories." After: "girt opens explicitly selected SHA-1
  repositories with supported files-backend references; unsupported extensions return errors."
- Before: "This section explores our correctness-first compatibility story." After: "Each supported
  Git feature must preserve Git's formats and semantics."
- Before: "Use nightly. Format Rust." After: "`just fmt-rust` uses nightly rustfmt because
  `rustfmt.toml` enables unstable options."

## Evidence and Handoffs

Keep an artifact when it supports a claim, review, reproduction, or unresolved task. Ask what a
reader or the next worker would do with it tomorrow. Retain exact revisions, fixture provenance,
failure records, and logs when they answer that question. Consolidate repetitive status, command
transcripts, and test-count inventories into a short linked result. Archive superseded evidence with
an index when readers still need its history; preserve stable links and the original facts.

A handoff should state the delivered behavior, material decisions and limits, the next action, and a
concise validation reference. Put detailed evidence at the linked source so a reader can inspect it
without making the handoff a work diary. User guides should explain the current operation and its
recovery obligations; keep roadmap history in roadmap or evidence pages.

## Validation

- For final review of substantive prose changes or tone-focused edits, load
  [Unslop Review](unslop.md). Typo and link corrections do not require that pass.
- For changed Markdown, follow `.config/rumdl.toml` for 100-column prose. Run `just docs-check`
  (rumdl and scoped Vale); use `just fmt-md` to fix formatting.
- For Rust library contracts and examples, also apply [Rustdoc Standard](rustdoc.md).
- Report unavailable checks honestly. Recheck after fixes; stop once relevant checks pass and known
  correctness issues are resolved.
