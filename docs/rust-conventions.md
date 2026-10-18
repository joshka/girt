# Rust Conventions

Use these conventions alongside the [Rustdoc Standard](rustdoc.md) and
[Testing and Completion Criteria](testing.md).

## Module Roots and Ownership

- Treat `lib.rs` and directory-root modules primarily as tables of contents: module declarations,
  re-exports, and a concise overview.
- Put definitions in files named for the concepts they own.
- Re-export items where that makes the public API easier to navigate. Public modules should provide
  useful namespaces rather than expose every implementation file.
- Keep detailed contracts, examples, and limitations with their owning type or operation. A reader
  arriving directly at a type should not need the crate overview to understand its obligations.
- Keep filesystem assumptions and storage errors with loose storage; link to those contracts from
  the crate root.
- Keep defaults and serialized initial metadata owned by their initializer. Composing workflows
  should reuse those values for exact-byte or expected-value preconditions instead of reconstructing
  them; otherwise a formatting or default change can break a later workflow phase.

Configured refspec/config/environment orchestration belongs in `workflow`. Its free functions take
an explicit repository; the core `Repository` API exposes Git concepts and storage operations. jj
adapters wanting command-like resolution call `workflow::fetch`, `workflow::push` and
`workflow::remote_head`, importing their options, callbacks and environment from that namespace.
Callers that own transport scheduling and publication use the lower-level `fetch`/`push` artifacts.

`KnownHistory::new` and the receiver-root-aware `PreparedPush::new` work for either format and
transport; constructor aliases must not suggest different validation guarantees. jj should use empty
receiver roots for a full push and explicit verified roots for an incremental one.

Native fetch separates `LocalFetchRequest` history/bounds, `TransportControl`, and synchronous
`LocalFetchObserver` callbacks. Keep messages, source construction and validation distinct, and keep
runtime, retention and publication state outside these values. jj can retain its configured
`workflow::fetch` adapter or use explicit requests when it owns scheduling and publication.

`LocalPushRequest` binds a prepared artifact to inherited receiver configuration and reflog
identity. The request exposes separate configuration and identity setters; control and validation
observers remain separate. jj's workflow adapter supplies its existing configuration/identity
policy. Avoid a context that also owns transport runtimes, retention or publication state.

## Default and Explicit Forms

Limits and cancellation are caller decisions, but most callers want the defaults. Name the forms
consistently so each is easy to find next to the others:

- `verb(inputs)` is the default form. It passes `Default::default()` for every limits argument and a
  flag that is never set. It never uses `trusted()` bounds, so it is as safe on untrusted input as
  the explicit call with default limits. Its Rustdoc names the `_with` call it wraps.
- `verb_with(inputs, limits)` is the explicit form. When the operation is cancellable it also takes
  the flag: `verb_with(inputs, limits, cancel)`.
- `verb_controlled(inputs, limits, cancel)` remains the cancellable form for operations whose
  `_with` form takes no flag, such as `Objects::read_controlled`.

The default form drops only limits and cancellation; choices such as a status baseline or a checkout
target remain arguments. Give common entry points this pair, not every operation that takes limits;
an operation without a default form keeps its plain name with explicit arguments. Typed reads
(`read_commit`, `read_tree`, `read_tag`, `read_blob`) follow the same pattern and treat absence as
an error. Callers that never cancel an operation without a default form pass
`&AtomicBool::new(false)`; do not export a shared never-set static, because any caller could set it.

## Vertical Spacing

Readers scan code by its shape: a blank line marks the edge of a group of related statements, so a
body with no gaps reads as one undifferentiated step even when each line is clear. Write function
bodies, tests and examples as short paragraphs, each doing one step of the work.

- Separate setup, the operation, and checking or building the result. In tests this is arrange, act,
  assert.
- Put a blank line before and after a multi-line `if`, `match`, loop or long call, unless it
  directly consumes the line above it, such as `let entry = ...;` followed by `match entry`.
- Keep a binding next to the code that uses it; do not split one step across paragraphs.
- Put a comment that introduces a paragraph directly above it, after the blank line.
- Short bodies of about five lines rarely need a gap. Never add blank lines after an opening brace,
  before a closing one, or two in a row.
- Doctest examples follow the same rule. Hidden `#` lines do not count; a blank line goes between
  visible groups.

A body that needs many paragraphs may also want named helpers; spacing is not a substitute for that,
but it should come first because it is cheap and reveals the structure. Run
`scripts/code-spacing.py` to list paragraphs of more than ten lines with two or more statements in a
block. A single long expression, `match` arms and struct literals are not flagged, and a flagged
paragraph that genuinely is one step may stay as it is.

## Domain Types at API Boundaries

- Use domain types when a primitive would admit invalid choices, obscure meaning, or allow unrelated
  values to be interchanged.
- Use an enum for a finite set of meaningful alternatives, such as Git object formats.
- Use a newtype when a value needs validated construction or an identity distinct from its
  underlying representation, such as an object identifier.
- Keep primitive types when their meaning and valid values are already clear and a wrapper would add
  no useful guarantee.
- Parse external strings at the boundary, then pass the domain type through library operations.
- Avoid magic strings such as `"sha1"` to select object behavior. A newtype around an unchecked
  string does not solve that problem.
- Distinguish recognized values from supported operations. Explicitly reject a recognized format
  when the storage implementation does not support it.
- Do not introduce an extensible format registry or speculative configuration framework to represent
  a small closed choice.

Use `ignore::InputKind` for filesystem kind, `fetch::AdvertisementKind` for tip versus peeled hint,
and `refs::Dereference` for the target of an edit. jj traversals choose input kind explicitly; jj
conditional stored-name edits select `Dereference::Stored`. `remote::ForceIntent` describes a
refspec's request, while `push::ForcePolicy` authorizes an update. Do not conflate these policies.
Protocol initiation uses `remote::ProtocolOrigin`; process environment decoding preserves the
existing user/automatic semantics. Keep named setters for independent one-bit state.

## Representation and Compatibility

Design APIs around cohesive Git concepts and operations. Consumer methods establish requirements;
their signatures and layering need not become girt's architecture. Prefer simple idiomatic Rust and
derive standard traits when their semantics fit. Keep exact representation separate from interpreted
values: preserve object/header/path bytes and unknown fields where the contract requires them. Use
OS path types at filesystem boundaries and explicit encoding conversions. Do not inherit a
consumer's unnecessary UTF-8 restriction or silently normalize Unicode, case, or signed content.

Characterize parsing separately from operational validation. Supported semantics must agree with Git
observations; do not reject Git-accepted data merely to simplify library invariants. Name the Git
operation used as the oracle: storing raw bytes, reading an object, and validating it can have
different acceptance rules. Choose practical deterministic ambiguity resolution and document it.
Safety, resource, and unsupported restrictions need evidence, explicit errors, and a GitHub issue
when they block required compatibility. No finite corpus establishes every edge case.

## Public Type Evolution

Use `#[non_exhaustive]` wherever a public type can reasonably change over time. Adding variants or
fields to these types must not force another breaking release. Callers match extensible enums with
an explicit fallback and construct extensible structs through supported constructors. Marking a
previously exhaustive type non-exhaustive is itself breaking, so decide when introducing the type.

- Mark public error enums and their cause/kind enums non-exhaustive almost always.
- Mark other public enums non-exhaustive when their set can grow, including hash/object formats,
  protocols, capabilities, policies, maintenance outcomes and progress phases.
- Leave an enum exhaustive only when growth is truly implausible, and justify the closed set in its
  Rustdoc. `ObjectKind` represents Git's four fundamental object kinds; `index::Stage` represents
  the four values of an on-disk two-bit field. Both remain exhaustive.
- Mark public structs non-exhaustive when they are likely to gain fields, including options, limits,
  reports, outcomes, statistics and error context. Inputs need `Default` plus `with_*` methods, or
  `new` for required data; preserve direct field mutation when useful. Read-only reports need only
  the attribute. Opaque structs with private fields already permit field additions through their
  existing construction APIs.
- Leave fixed-shape data and exact format records alone, such as validated name newtypes,
  signatures, timestamps and raw reftable records. An object identifier enum that selects hash
  formats can grow even though each individual digest has a fixed shape.

Keep fallback behavior explicit: propagate unknown errors, reject unsupported operational choices,
and report unknown progress or outcomes conservatively. Do not silently treat a future variant as
success. Non-exhaustiveness permits compatible additions; it does not make removals, field type
changes or changes to existing semantics compatible.

## Public Auto Traits

Preserve the auto traits a public type implements in a published release: `Send`, `Sync`, `Unpin`,
`UnwindSafe`, and `RefUnwindSafe`. Losing one changes which downstream code compiles and requires a
deliberate breaking release. `Objects`, `RetentionPlan`, and `KnownHistory` promise both unwind
traits as well as `Send` and `Sync`; keep compile-time regression checks for these cache owners.

Review private field changes for their effect on these traits. Keep unwind-safety assertions at the
smallest cache boundary and explain why a panic cannot publish a partial or misleading result.
`AssertUnwindSafe` is appropriate for immutable results published atomically after successful
initialization. For multi-step mutation, reset or poison-guard the cache before readers reuse it. Do
not assert traits on the whole public type to hide a new field's unresolved invariants.

## Errors

- Use `thiserror` to derive library error implementations. Keep concrete error types and variants
  owned by their domain modules.
- Preserve underlying causes with `#[source]` or `#[from]` where appropriate so callers can inspect
  failures without parsing display text.
- Either display a cause or return it from `source()`, never both. Callers such as jj render the
  whole chain by printing each error and then walking `source()`, so interpolating the source into
  the message shows it twice. Describe only this layer's context, such as `reading commit {id}`, and
  use `#[error(transparent)]` only for wrappers that add no context.
- Distinguish missing, corrupt, unsupported, resource-limited, cancelled, and uncertain outcomes
  where callers need different actions. Preserve recoverability, retry preconditions, and partial
  effects in structured results. Never recommend retrying an uncertain mutation without checking
  state. Public errors need not imitate Git's CLI text.

Maintenance observations use `retention::RetentionError` for failed bounds, unsupported inputs,
changed generations and graph inconsistencies. Preserve underlying read errors through `source()`;
jj chooses recovery from `RetentionError::kind()` and the detailed condition and renders the source
chain once. Box large cause payloads, as `ExpireCause::Incomplete` does, so partial reports stay
compact across platforms without suppressing size lints. Do not restore free-text `Incomplete`
payloads when composing maintenance phases. Retained diagnostic sources keep `Send + Sync` but do
not promise unwind safety; do not assert unwind markers for arbitrary I/O causes.

## Observability Ownership

Return errors to the operation owner, which decides user-visible logging and severity. Optional
instrumentation may expose operations, elapsed work, counts, cancellation, resource use, and failure
classes. The caller owns subscribers, filtering, runtime, and global policy. Avoid duplicate error
logging and event/schema frameworks without a demonstrated need. Do not record credentials, raw URLs
with secrets, environment values, paths, identities, object contents, or signature payloads by
default. Test redaction and disabled-instrumentation behavior at the owning boundary. Follow the
[tracing contract](tracing.md#cost-and-future-instrumentation) when adding operation spans,
including explicit span handles and suspension/worker ownership tests.

## Unit Tests

- Keep unit and integration test bodies simple: setup, the operation, and direct assertions.
- Avoid `if`, `match`, `for`, and `while` in test bodies when they select scenarios or expected
  results. Use `rstest` with named `#[case::name(...)]` inputs that supply the expected values; do
  not reimplement the algorithm under test to calculate them. Keep plain `#[test]` functions for
  single scenarios.
- Put reusable fixture construction in small, named helpers. Keep logic that is intrinsic to the
  behavior being tested, such as coordinating concurrent writers, explicit and minimal; do not move
  scenario-selection logic into helpers merely to hide it.
- Add succinct purpose comments to tests and fixture helpers when their intent is not clear from the
  name. Explain the behavior, boundary, or failure being checked rather than narrating test steps.
- Cover new behavior and API contracts with focused unit tests beside the implementation, including
  rejected inputs and promised absence of side effects.
- Use integration tests for behavior across components and Git interoperability; do not rely on them
  alone for independently testable local behavior.
- Assert observable behavior and invariants rather than reproduce implementation steps or test
  derived traits. Keep cases focused on meaningful regressions.

## Synchronous Storage and Async Network Boundaries

Local storage, parsing, encoding, hashing, compression and graph algorithms are synchronous and
independent of any async runtime. Do not make them async because a network caller uses them, and do
not add an async storage trait or adapter without a consumer that needs remote object lookup or a
concrete storage responsiveness guarantee. A whole-operation blocking-worker adapter would be the
first option to consider then; async integration does not require rewriting the core.

Use async I/O where waiting for a remote peer must remain responsive to cancellation and deadlines.
Keep the runtime owned by the caller; isolate network dependencies behind the `http` and `ssh`
features.

Separate potentially substantial synchronous work from async network calls. HTTP and SSH fetch
return an owned bounded download whose explicit `validate` method imports the pack and checks
connectivity. Push preparation remains synchronous and precedes sending; sending consumes the
prepared buffers without copying the pack. This keeps executor scheduling and CPU concurrency under
caller control without creating detached blocking jobs whose cancellation and resource lifetime
would be hidden. Callers should use a bounded worker pool for large validation/preparation/install
operations. See [HTTP contracts](http.md) and [SSH contracts](ssh.md) for current boundaries.
