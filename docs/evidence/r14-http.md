# R14 Windows HTTP Validation Repair

## Acceptance Contract

This follow-up preserves R14's storage implementation and evidence. It investigates the three
Windows HTTP post-send fault tests that failed twice during R14 validation, then repairs the cause
without changing uncertain-outcome semantics to accommodate a fixture.

- Retain the actual structured `PushError` and observed request sequence on failed assertions.
- Establish whether the failure occurs before or after POST, using native Windows evidence.
- Inject stalled-POST cancellation only after the fixture observes POST; assert the specific cause,
  pending reference outcomes and absence of retry. Keep meaningful deadline coverage separate.
- Run affected HTTP and tracing regressions locally and in a focused Windows job. Reuse R14's
  unaffected native pack/index/reference and Unix results rather than repeating those matrices.
- Retain exact revisions, commands and original failed attempts. Keep the isolated missed tracing
  span with C02 unless evidence establishes a shared cause. Dispatch no successor and perform no
  merge or release.

## Preserved Evidence

R14's executable revision is `5efeb3d142dba943af7430f26e86fa49affc0b7b`; its evidence child is
`24d06505a4f6170844b822f0fd8808b5ba03d183`. The original broad run and Windows retry are retained in
[R14 evidence](r14.md#windows-retry-and-acceptance-blocker). Both failed the three HTTP push fault
cases at the old `tests/http.rs:645`, before the assertion included the actual error.

The diagnostic revision is `7e919766cc89a66b35579b76e429f6748e43e14b`. Its
[focused Windows run](https://github.com/joshka/girt/actions/runs/36157961647) uses the existing
platform workflow's new `http_only` manual input. Automatic push CI is skipped for this scoped
bookmark; manual dispatch executes only Windows HTTP/tracing and scoped Clippy. An earlier
diagnostic dispatch was canceled after discovering that the added assertion was at the wrong call
site; it supplies no test evidence.

## Diagnosis and Repair

The instrumented diagnostic run passes all 37 Windows HTTP and seven portable HTTP cases without
changing their behavior. It reproduces the independent missing `fetch.http` span (29 tracing cases
pass, one fails). Consequently, the errors discarded by the two original failed assertions cannot be
recovered from those logs. This report does not claim their exact causes are known.

The fixture defect is independently reproducible: a one-second overall deadline does not guarantee
that discovery finishes before the intended POST fault. Three controlled cases delay discovery by
two seconds while retaining the original `post-failure`, `post-stall` and `post-media` modes. They
assert and print `PushError::NotSent(PushFailure::Deadline)` and verify that no POST occurs. This is
the library's documented preflight behavior, not a defect in uncertain-outcome classification.

The repaired post-send cases use separate fixture watchdogs instead of an injected deadline racing
with discovery. They verify the actual POST, one GET/one POST without retry, pending reference
outcomes, and specific causes: HTTP 503, invalid media type, or cancellation. The stalled-POST case
waits for the server's request log before setting cancellation. Ordinary and deliberately slow
(two-second discovery) variants demonstrate that all three faults reach their intended phase.
Existing discovery/TLS deadline checks and the post-response partial-acknowledgement deadline test
remain in the affected suite. No library or public API behavior changes.

The final manual `http_only` job covers the repaired HTTP and portable suites plus scoped Clippy.
The tracing fixture has no demonstrated connection to the discovery deadline race; its diagnostic
failure remains with C02 under the coordinator's explicit instruction. Native tracing success from
R14 attempt 2 and the later diagnostic failure are both retained; the focused HTTP repair run is not
represented as a new passing tracing run.

## Local Validation

On macOS 26.6.2 arm64/APFS with Rust 1.98.1 and Git 2.55.0, the changed HTTP suite passes 48 cases
and portable HTTP passes 11 (including Unix-only fixture-process tests). Scoped HTTP Clippy passes;
the unchanged HTTP/tracing suite passes 30 cases. Nightly Rustfmt and actionlint pass. The original
R14 codec/storage and native platform evidence remains applicable because library source is
unchanged. This test-only repair makes no performance claim and adds no dependency.
