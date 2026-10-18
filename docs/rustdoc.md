# Rustdoc Standard

Apply [Documentation Standard](documentation.md) for prose quality. These requirements cover
affected APIs and docs, including experimental APIs. Preferences are explicit; local changes do not
require crate-wide passes.

## Teaching and Ownership

- Keep README focused on purpose, status, setup, and canonical documentation. Teach Git concepts,
  workflows, entry points, and limitations in crate Rustdoc; distinguish guarantees from goals.
- Explain module ownership and relationships. Establish domain concepts before representation
  details.
- Place shared rationale on its owning type or module and local exceptions beside the operation.
  Explain surprising private helpers, ordering, and representations when they affect maintenance.
- Trace construction, mutation, and failure paths before claiming an invariant.

Teach configured-remote resolution through `workflow` examples with an explicit repository. Explain
its synchronous runtime and credential ownership for jj adapters. Core fetch/push examples should
keep caller-selected plans, controls, artifact validation and publication visible; link to
`workflow` when automatic configured resolution is useful.

Show receiver roots explicitly in `PreparedPush::new`; `&[]` requests a full transfer. jj adapters
use the same preparation contract for native and wire transports. Observer examples use
`PreparedPush::new_with_progress`; the local constructor aliases are removed.

Native-fetch examples construct `LocalFetchRequest` and show controls and observers separately. Use
`LocalFetchObserver::default()` when notifications are unnecessary. Source construction and
validation completion do not establish installation or publication, including in jj adapter docs.

Native-push examples construct `LocalPushRequest`, then pass control and a validation observer.
Explain inherited receiver configuration and reflog identity through `LocalPushRequest`, including
jj adapter defaults. Validation completion precedes hooks, installation and reference publication;
partial reports remain authoritative when a later step fails.

## Module Front Pages

A public module should let a newcomer choose an API before learning its representation. Explain what
problem it solves, when to use it, its mental model, and how its main types fit together. Include a
runnable task example with an observable result and links to the next operation. Document
substantial failure causes and remedies, and the performance or safety boundaries that change a
caller's decision. A list of item names alone is not an overview.

Place useful examples on main types and operations as well as module pages, so readers arriving from
search can act without reconstructing a workflow. Prefer examples that compose related APIs over
examples that only repeat an accessor's return value. Example coverage is a discovery aid, not a
reason to add repetitive snippets to every getter.

Keep docs.rs self-contained for concepts and contracts. Website guides teach tasks end to end,
including setup and recovery, and link to docs.rs for signatures and detailed guarantees. Avoid
copying the reference into guides; duplicated contracts drift independently.

## Comment Spacing

- Follow every Markdown heading with one empty doc-comment line before its content.
- Use one empty doc-comment line between sections or paragraphs; avoid consecutive empty doc-comment
  lines. This keeps source spacing consistent with the rendered document.

## Caller Contracts

- Begin with what the item does or represents. Give readers arriving from search or IDE hovers
  enough local context; link shared explanations without requiring a front-to-back reading.
- State behavior, ownership and borrowing, valid values, invariants, and relevant lifecycle or side
  effects. Include public fields and enum variants.
- Distinguish accepted input, validated properties, and preserved representation. Where relevant,
  specify byte/encoding handling, normalization, round-trip guarantees, and unsupported Git formats.
- When parsing and validation are separate, explain why and when callers need each operation. Show
  what parse success guarantees, what remains unchecked, and what callers retain after a validation
  failure. Examples should identify redundant checks rather than imply they are required.
- Explain feature, platform, compatibility, and API-stability constraints when relevant.
- Document failure conditions, partial changes, and recovery under `# Errors`, panic conditions
  under `# Panics`, and unsafe caller obligations under `# Safety`, where applicable.
- Justify safety obligations in local unsafe-operation comments; enforce them in safe wrappers.
  Current callers following a convention do not prove every permitted call is safe.

Hypothetical parser wording, only if verified: replace "Parses names safely" with "Returns a name
borrowing the input bytes; does not validate UTF-8." The latter preserves the ownership and
validation boundaries callers need.

Maintenance examples should use `RetentionPlan::is_complete()` or match the typed
`RetentionOutcome`; failure examples should demonstrate matching `RetentionError` and preserve its
source chain for jj diagnostics. For boxed expiry details, use `as_ref()` to borrow the
`RetentionError` before matching. Plans retain `Send + Sync`, but source errors do not promise
`UnwindSafe` or `RefUnwindSafe`; document that restriction when discussing panic recovery. An
incomplete plan never authorizes deletion, and partial mutation reports still require fresh
observations under isolation before retrying.

Policy examples should spell out `InputKind`, `AdvertisementKind`, `Dereference`, `ForceIntent` and
`ProtocolOrigin` choices. Explain defaults and distinguish a syntactic force request from permission
to publish. Show jj adapters deriving filesystem kind at traversal boundaries and selecting
stored-name edits without following symbolic references.

## Examples and Validation

- Group example code into paragraphs separated by blank lines, as described in
  [Vertical Spacing](rust-conventions.md#vertical-spacing).
- Demonstrate useful behavior with expected results or error handling. Prefer grouped or linked
  examples over repetitive accessor doctests. Distinguish example policy from library guarantees.
- Keep imports, dependencies, features, and lifecycle consistent across README, Rustdoc, and
  runnable examples. Update affected surfaces together.
- For repository-writing examples, use disposable repositories and show setup, mutations, and
  cleanup.
- Prefer executable doctests. Use `no_run` when execution needs external resources; explain any
  unavoidable `ignore`.
- For changed examples or contracts, run `cargo test --doc` and affected examples under documented
  features. For changed Rustdoc links, structure, or API surface, build with `just docs-rs` to check
  the docs.rs build with warnings rejected. Corrections preserving meaning, links, and examples need
  only prose review.
- Inspect rendered pages when layout or navigation changes. For internal documentation, also run
  `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --document-private-items`. Report unavailable
  checks honestly.
