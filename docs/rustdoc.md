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

## Examples and Validation

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
