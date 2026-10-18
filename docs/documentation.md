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
style rules mechanically when they obscure a technical contract.

## Library Learning Path

A developer should be able to move from the crate front page to a useful task, then to the API
contract for each step. Explain why an operation exists, when to choose it, and what it leaves to
the caller. Use realistic examples with expected results and explain mistakes a reader is likely to
make. This keeps documentation useful beyond an inventory of public items.

The crate front page owns orientation, the module map, feature selection, and a minimal first use.
Module pages own shared mental models and API choices. Item pages own precise contracts and local
examples. Website guides compose tasks across modules and link to the reference. This division keeps
essential explanations available on docs.rs while giving longer tasks room to develop.

## Command and Task Pages

Site pages that teach commands or tasks, such as the
[CLI section](https://www.joshka.net/girt/cli/), are read by someone trying to do one thing, often
arriving from search. Each page should let that reader find the command and finish the task without
leaving it.

- Give each distinct task its own section, and name the task and the command or commands that do it
  in the heading: "Move a reference safely (`girt ref set`)". Readers scan headings and the table of
  contents for the command they already know. Closely related commands, such as `object id`, `info`
  and `show`, can share a section.
- Show every command with an example whose output came from running it. Build examples on a fixture
  the pages show how to recreate, with fixed identities, dates and configuration so that IDs match,
  and show setup steps even when they use `git`. Rerun the examples in page order after editing.
- Document the command instead of pointing at `--help`: what it prints, the options that change
  that, its exit status, and what it refuses and why. `--help` output exists only in a terminal, so
  a reader can't reach it from a search or a link.
- Explain output only where a field is not self-describing. `bare: false` needs no sentence;
  `retained`, which applies reflog expiry, does.
- Put library calls and Git equivalents beside the example they belong to, not in a closing
  paragraph. Check every Git equivalence and `--git` claim against the source or the executable, and
  name the exceptions where they apply.
- Write in the reader's vocabulary. Words from the implementation, such as "candidates",
  "capabilities", "publication" or "retention inputs", need translating into what the reader sees or
  does.
- Keep one copy of any notice repeated across pages, as a shared component.

## Evidence and Pull Requests

Put the evidence for a change in its pull request description or commit messages: exact revisions,
Git versions, fixture provenance, benchmark results and anything that could not be checked. Tracked
docs keep only what a current reader needs, such as contracts, limitations and the benchmark table;
remove spent process notes, command transcripts and test counts from them. Retained evidence under
`docs/evidence/` is historical and stays dated.

Pull request descriptions and replies to the maintainer should be plain and short. Lead with the
result and any blocker, then the behavior changed, the evidence, and the limits. Link detail rather
than repeating it, and do not narrate the work.

## Validation

- For final review of substantive prose changes or tone-focused edits, load
  [Unslop Review](unslop.md). Typo and link corrections do not require that pass.
- For changed Markdown, follow `.config/rumdl.toml` for 100-column prose. Run `just fmt-md-check`
  (rumdl); use `just fmt-md` to fix formatting.
- For Rust library contracts and examples, also apply [Rustdoc Standard](rustdoc.md).
- Vale checks spelling and selected terminology; passing it does not establish accuracy or a usable
  structure.
- Report unavailable checks honestly. Recheck after fixes; stop once relevant checks pass and known
  correctness issues are resolved.
