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

## Reader Tasks and Organization

Every page needs an identifiable reader and purpose: learning a first workflow, completing a task,
looking up a contract, or understanding a design decision. Use [Diátaxis](https://diataxis.fr/) to
separate these needs without creating four empty sections for every topic. Library users and
contributors need distinct entry points. Historical evidence belongs in linked reports, not in the
getting-started path.

Support three reading modes: a short ordered path for newcomers; self-contained API and task pages
for direct arrivals; and informative headings, opening sentences and links for scanning. Put the
reader's vocabulary and distinguishing concepts early. Link labels should predict their destination,
as described by [information scent](https://www.nngroup.com/articles/information-scent/). Keep
related prerequisites, effects and recovery advice close to the relevant operation.

Prefer useful information per sentence over minimum word count. Remove duplicated setup, stale
scaffold claims, unsupported promises and conversational residue. Preserve explanations that prevent
mistakes. Use consistent Git terminology, specific examples and a calm, direct voice. Avoid applying
style rules mechanically when they obscure a technical contract. Google's
[style highlights](https://developers.google.com/style/highlights) and Microsoft's
[voice guidance](https://learn.microsoft.com/en-us/style-guide/top-10-tips-style-voice) inform
editorial judgment; local conventions resolve conflicts.

## Whole-Project Documentation Review

Inventory README, guides, contributor material, examples, public Rustdoc, diagnostics and release
text. For each surface, record its reader, task, canonical owner and current accuracy. Consolidate,
move or remove material when that reduces navigation and duplication; do not expand every topic into
a page. Keep historical evidence clearly dated and separate from current instructions.

Review using concrete journeys: install and read an object; mutate refs and recover from failure;
configure a transport; understand supported formats and limitations; contribute and run checks. Test
both ordered reading and entry through an API search result. Record broken steps, misleading
headings, unnecessary navigation, missing prerequisites and terminology drift. Fix high-impact
findings, then reread the changed journeys and inspect the rendered pages. A second editorial pass
should challenge the first draft's organization and assumptions, not merely polish its sentences.

Use [Vale](https://docs.vale.sh/) for selected high-signal rules and a project vocabulary. Review
Google/Microsoft style packages for conflicting rules and false positives before enabling them.
Scope checks to prose, preserving code and protocol literals; document justified exceptions. Passing
Vale does not establish accuracy, a coherent voice or usable information architecture. Completion
requires runnable examples, working links, verified claims and the reader journeys above, with
remaining findings explicitly assigned. Stop when remaining edits only exchange equivalent wording;
documentation volume and lint counts are not quality targets.

## Evidence and Handoffs

Keep an artifact when it supports a claim, review, reproduction, or unresolved task. Ask what a
reader or the next worker would do with it tomorrow. Retain exact revisions, fixture provenance,
failure records, and logs when they answer that question. Remove spent process notes, command
transcripts, and test-count inventories from tracked docs. Preserve useful history in private local
storage outside tracked paths when needed, then repair links so published pages stand alone. Keep a
compact index only when readers still need to locate retained evidence; do not leave placeholder
pages.

A handoff should state the delivered behavior, material decisions and limits, the next action, and a
concise validation reference. Put detailed evidence at the linked source so a reader can inspect it
without making the handoff a work diary. User guides should explain the current operation and its
recovery obligations; keep roadmap history in roadmap or evidence pages.

## Validation

- For final review of substantive prose changes or tone-focused edits, load
  [Unslop Review](unslop.md). Typo and link corrections do not require that pass.
- For changed Markdown, follow `.config/rumdl.toml` for 100-column prose. Run `just fmt-md-check`
  (rumdl); use `just fmt-md` to fix formatting.
- For Rust library contracts and examples, also apply [Rustdoc Standard](rustdoc.md).
- Report unavailable checks honestly. Recheck after fixes; stop once relevant checks pass and known
  correctness issues are resolved.
