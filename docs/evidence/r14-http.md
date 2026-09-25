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

## Native Verification

The repair revision is `42a724ab865976526130b3d536c6a3e82b0dcbe3`, a separate atomic child of R14's
evidence change. The
[focused Windows verification](https://github.com/joshka/girt/actions/runs/36158949345) executes
only the HTTP/portable suites and scoped Clippy. All ordinary platform jobs are skipped by the
explicit manual input; automatic push matrices are skipped by the validation change's commit
message. This avoids rerunning unaffected R14 coverage.

[Repair source fingerprints](r14-http-source.sha256) identify the tests, fixture, workflow and
manifests. R14's library, pack-test and manifest fingerprints still match; its workflow fingerprint
necessarily differs because this repair adds the manual focused job. No prior fingerprints or
failed-run logs are overwritten. The focused native job passes 43 HTTP cases, seven portable HTTP
cases and scoped Clippy on Windows Server 2022 Datacenter 10.0.20348/NTFS, Rust 1.98.1 and Git
2.55.0.windows.5. All three controlled discovery-delay cases print
`NotSent(Deadline); requests: ["GET"]`. All three repaired slow-discovery post-fault cases pass. The
existing discovery/TLS and partial-acknowledgement deadline cases also pass. No third broad matrix
or unrelated index/reference matrix was launched.

## Completion and Remaining Ownership

The HTTP fixture defect is repaired without changing library semantics. R14 is ready for coordinator
acceptance using its existing native storage evidence plus this focused Windows result. The original
broad workflow remains historically failed; this follow-up does not rewrite that result or claim a
new full-matrix pass. The exact unexpected errors from its original assertions remain unknowable
from the retained output. The controlled native experiment establishes the preflight deadline
counterexample and the repaired tests verify the intended post-send behavior.

The missing tracing span remains explicitly assigned to C02. It passed in R14's Windows retry and
failed again in this follow-up's diagnostic run, while local tracing passes. No shared cause with
HTTP discovery timing was demonstrated and no tracing code or assertion was changed.

[Artifact fingerprints](r14-http-artifacts.sha256) cover the diagnostic/verification metadata, logs
and local checks under
[/Users/joshka/.codex/reports/girt-r14-http](/Users/joshka/.codex/reports/girt-r14-http). The
canceled initial diagnostic supplies no evidence. R14's original failed attempts and fingerprint
manifests remain intact. No successor, merge or release is performed.
