# Testing and Completion Criteria

Define completion for each usable capability before implementing it. Name the supported behavior,
compatibility boundaries, and evidence needed to accept the change. Select tests by the promises and
risks involved; one test can satisfy several criteria without duplicating assertions across suites.

## Required Evidence

- **Behavior:** State what callers can do, supported formats, and explicit exclusions. Reject
  unsupported cases rather than silently approximate them.
- **Local correctness:** Cover independently testable rules with focused unit tests beside the
  implementation. Include meaningful boundaries, rejected inputs, and promised absence of side
  effects. Assert observable behavior rather than implementation steps or derived traits.
- **Public API:** Demonstrate a usable operation through an executable example or integration test.
  Exercise ownership, errors, and setup as a library consumer would.
- **Git compatibility:** Compare with Git when reading, writing, or interpreting Git data. Exercise
  both directions where applicable. Round trips alone can hide a bug shared by reader and writer;
  include independent expected values or Git-generated input.
- **Failure behavior:** Test relevant corruption, resource limits, partial writes, cleanup, and
  concurrency guarantees. Document what may remain after failure.
- **Documentation:** Update affected contracts, examples, status, compatibility evidence, and
  limitations. Record fixture provenance and the Git versions and platforms exercised.
- **Validation:** Run the applicable [development checks](../CONTRIBUTING.md#development-checks).
  Fix failures caused by the change and report checks that could not run.

Keep integration tests under `tests/` and original fixtures under `tests/fixtures/` when files are
needed. Generate fixtures independently and isolate filesystem tests from the working checkout and
global configuration. Follow [Rust Conventions](rust-conventions.md#unit-tests) and the
[Rustdoc Standard](rustdoc.md#examples-and-validation) for local tests and executable examples.

## Performance Evidence

Add a reproducible benchmark when introducing a significant processing path, changing a known hot
path, or making a performance claim. Hashing, compression, pack decoding, and graph traversal merit
representative baselines. A domain-type change or documentation correction usually does not require
a new benchmark; use existing benchmarks when performance could be affected.

Keep benchmark harnesses under `benches/`. Choose representative input sizes and content, keep
fixture setup outside the measured operation, and name what is measured. Separate operations with
different costs, such as publishing a new object and validating an existing one. For filesystem
measurements, record cache conditions and avoid presenting cached reads as cold-storage performance.
Record the command, toolchain, platform, workload, and baseline revision needed to repeat a result.

Use measurements to identify sustained regressions and guide design. Establish numerical acceptance
thresholds only when a consumer requirement or measured budget justifies them; noisy timing changes
should not automatically fail CI. Throughput measurements do not prove memory limits or bounded
resource use: validate those contracts separately. No numerical code-coverage threshold is required.

## Implementation Task Checklist

Include a concrete version of this checklist in each capability's task description. Replace general
items with named behaviors, inputs, and expected outcomes before work begins. Mark an item
inapplicable with a reason when the capability does not need it.

- [ ] Supported behavior and exclusions are stated.
- [ ] Local rules, boundaries, and rejection cases have focused tests.
- [ ] A consumer-facing example or integration test exercises the operation.
- [ ] Git compatibility has independent evidence in the applicable directions.
- [ ] Relevant failure, resource, and concurrency guarantees are tested.
- [ ] Performance-sensitive work has a reproducible baseline or comparison.
- [ ] Documentation and compatibility evidence match the implementation.
- [ ] Applicable checks pass; untested platforms and other limitations are recorded.

A capability may be complete within an explicitly limited scope. Validation on one platform does not
establish support on other platforms, and recognizing a format does not establish that it is
implemented. Keep these distinctions visible in the completion report.

## Blob Baseline Follow-up

The [blob compatibility record](compatibility.md) describes the existing integration coverage.
Additional baseline work should include focused tests for exact blob headers at decimal-length
transitions, preservation of binary bytes, and independently established expected identities. Add
benchmarks for blob hashing and loose reads/writes using small and larger inputs with different
compressibility, separating new-object writes from writes to existing objects. These are follow-up
criteria, not claims that the tests or benchmarks already exist.
