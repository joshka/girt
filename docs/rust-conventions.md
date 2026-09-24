# Rust Conventions

Use these conventions alongside the [Rustdoc Standard](rustdoc.md) and the repository's
maintainability and testing guidance.

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

## Errors

- Use `thiserror` to derive library error implementations. Keep concrete error types and variants
  owned by their domain modules.
- Preserve underlying causes with `#[source]` or `#[from]` where appropriate so callers can inspect
  failures without parsing display text.

## Unit Tests

- Keep unit and integration test bodies simple: setup, the operation, and direct assertions.
- Avoid `if`, `match`, `for`, and `while` in test bodies when they select scenarios or expected
  results. Prefer named `rstest` cases with explicit inputs and expectations.
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

## Synchronous Storage and Async Deferral

- Keep the current experimental local loose-object API explicitly synchronous and blocking. Its
  scope does not yet justify an async runtime dependency, adapter, or backend framework.
- Keep parsing, encoding, hashing, and compression independent of an async runtime.
- Revisit the boundary before defining a shared object-store trait that consumers implement,
  allowing object lookup to fetch remote data, or integrating an async consumer with concrete
  responsiveness or concurrency requirements.
- At that point, decide the local-versus-remote boundary, owned buffers for worker dispatch,
  concurrency limits, and cancellation and write-completion semantics.
- Consider a whole-operation blocking-worker adapter for local storage; async integration does not
  necessarily require rewriting the core.
- Treat this as a deferred design decision, not authorization to add async or expand the supported
  storage scope.

## Async Network Boundaries

Use async I/O where waiting for a remote peer must remain responsive to cancellation and deadlines.
Keep the runtime owned by the caller; isolate network dependencies behind the `http` feature. Do not
make object formats, hashing, graph algorithms or local storage async solely because a network
caller uses them.

Separate potentially substantial synchronous work from async network calls. HTTP fetch returns a
bounded download whose explicit `validate` method imports the pack and checks connectivity. Push
preparation remains synchronous and precedes sending; sending consumes the prepared buffers without
copying the pack. This keeps executor scheduling and CPU concurrency under caller control without
creating detached blocking jobs whose cancellation and resource lifetime would be hidden. Callers
should use a bounded worker pool for large validation/preparation/install operations. Broader async
storage APIs remain deferred until a consumer needs remote object lookup or a concrete storage
responsiveness guarantee. See [HTTP contracts](http.md) for current boundaries.
