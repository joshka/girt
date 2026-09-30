# R35: Explicit Include Placement

`Config::resolve_with_include_placement` adds an opt-in `AfterSectionReverse` mode without changing
`ConfigInputs` construction or default Git ordering. The existing resolver validates and accounts
for sources forward and depth-first. Successful child blocks move after their complete physical
parent section, in reverse directive order. Physical section membership and source provenance move
with entries; empty and repeated sections remain distinct.

## Original Compatibility Evidence

Fourteen disposable public-gix API fixtures establish same-section reversal, separate-section
ordering, nested blocks, scalar entries between directives, matching and false `gitdir` conditions,
empty headers, repeated sources and malformed child precedence. Errors in the first child win over
later siblings, including errors reached through its nested include. Reversing traversal would
therefore change error behavior. The probe uses cached dependency binaries and original fixture
bytes; no upstream implementation or test source was read.

The temporary probe sources and results are in `/tmp/r35-legacy-native-profile`: `probe.rs`,
`includes.py` and `include-results.txt`. All 14 native differential cases pass against these same
original fixtures; `differential.py` and `differential-results.txt` retain the comparison. The
public option models placement only. It does not claim wholesale opener compatibility.
`RepositoryLocation::read_metadata_with_config_and_include_placement` explicitly selects this policy
for ordinary metadata reading. Existing metadata, full-open and command-layout methods retain their
default placement; direct bootstrap, layout and trust obligations are unchanged. Source profiles,
trust, platform qualification and consumer removal of the old fresh opener remain separate work.

## Validation Scope

Focused tests cover nested occurrence ownership, exact origin paths and lines, default ordering,
runtime input replay, empty/repeated headers, later-layer `hasconfig` matching and prohibited URLs.
A budget matrix compares both modes' admission and first-error locations under byte, entry and depth
limits. Temporary placement slots are bounded by the existing entry and section budgets; config
values are moved rather than cloned during placement.

Both-format metadata fixtures exercise inherited, local, private, environment and command scopes,
unchanged default readers and first-error include ancestry. The new entry point adds no source
selection or ambient environment reads.
