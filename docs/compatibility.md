# Git Compatibility Evidence

Each supported boundary is tied to the observations that established it. For current usage, start
with the [README examples](../README.md#examples) or the
[published API documentation](https://docs.rs/girt/latest/girt/). For task-specific instructions,
read [repository layouts](repositories.md), [configuration](configuration.md),
[references and recovery](reference-transactions.md),
[working-tree status and checkout](working-tree.md), [HTTP](http.md), and [SSH](ssh.md). The results
apply to their stated revisions. Paths under `/Users/joshka/.codex/reports/` identify
maintainer-local artifacts that are not part of this repository or published crate. The linked
`docs/evidence/` records retain the portable scope and revision summaries.

R11 extends the files backend to portable conditional refs and reflogs; its current
[contracts](reference-transactions.md) and [evidence](evidence/r11.md) supersede historical
Unix-only reference and strict imported-reflog restrictions below. R37 adds reftable under its
[backend evidence](evidence/r37.md), passing both-format native validation on Linux, macOS and
Windows. Historical validation records retain their original scope.

SHA-1 and SHA-256 codecs, loose/packed storage, references, reflogs and working-tree index v2/v3/v4
use the repository's configured object format. The [R04 evidence](evidence/r04.md) covers codecs and
loose storage; [R05 evidence](evidence/r05.md) covers pack/ref/index propagation, including Git
interoperability and failure contracts. [R26 discovery](evidence/r26.md) identifies both remote
formats through native local, HTTP, SSH and caller-owned protocol streams. R27 adds SHA-256 v0/v1
wire fetch with bounded receipt and verified thin-pack completion. R29 extends wire push to both
formats; native local push also supports both. No dual-hash conversion is provided.

Native local fetch and push use girt storage and references in both formats; see
[R25 evidence](evidence/r25.md). Callers must exclude external GC/pruning and worktree/HEAD changes
from preparation through publication; conditional ref locks do not enforce that exclusion. The
historical upload-pack, receive-pack and owned local-process sections below describe their original
validation revisions, not the current local adapter.

Explicit format arguments select standalone pack/index/reflog codecs, including empty artifacts.
Repository operations derive the format from common configuration, never from artifact lengths.
Crate Rustdoc owns current examples and caller contracts. Earlier sections below retain their
historical evidence scope; R04/R05 supersede their SHA-1-only storage and codec limits, and
[R07](evidence/r07.md) supersedes the earlier strict commit/tag/tree reading restrictions. The
[R06 evidence](evidence/r06.md) records the native Linux/macOS/Windows refresh for these increments,
including the selected Windows scope and exact tested revision.

## Reachability bitmap files

`girt::bitmap` reads bitmap v1 and reverse-index v1 files for SHA-1 and SHA-256 packs and multi-pack
indexes. Git CLI fixtures compare decoded type maps and selected-commit closures with
`git rev-list --use-bitmap-index --objects`; `.rev` mappings translate bitmap order into ordinary
index order. Git-written optional name-hash caches and lookup tables are accepted. XOR dependencies
are decoded under an expansion budget. Unsupported pseudo-merge extensions are refused explicitly.

`Objects::reachable_difference` compares include-minus-exclude object sets with Git for branched
SHA-1 and SHA-256 histories, pack bitmaps, and MIDX bitmaps spanning multiple packs. Zero walk-byte
fixtures prove covered roots use bitmaps. Corrupt sidecars, zero metadata-work budgets, uncovered
loose tips, missing roots, independent closure limits, tag/tree/blob roots, skipped gitlinks and
shallow boundaries are covered by `tests/bitmaps.rs`. Packed file handles remain pinned in results.

Checksums and structure establish association with an inventory, not the truth of graph reachability
claims. Forced walking provides an independent graph result. Reusing verified bindings across
queries is tracked in [the bitmap cache issue](https://github.com/joshka/girt/issues/717).
Incremental MIDX and alternate-store bitmap acceleration, pseudo-merges and bitmap writing remain
tracked in [the bitmap issue](https://github.com/joshka/girt/issues/518). See
[the reader guide](bitmaps.md).

The bitmap encoders produce pack and MIDX artifacts in both hash formats. Isolated
`git rev-list --test-bitmap` validates the selected commit graph and prints `OK!` for each generated
fixture, and `--use-bitmap-index --objects` agrees with decoded girt output. `git verify-pack` and
`git multi-pack-index verify` also accept their associated inventories. Output uses independent
EWAH records, lookup tables and reverse indexes; MIDX output includes RIDX/BTMP and unknown (zero)
pathname-hash hints. This establishes format and graph compatibility, not identical compression or
selection to Git.

Bitmap publication is covered by idempotent pack/MIDX writes, shared writer-lock exclusion,
pre-publication failure preservation and cancellation. The CLI accepts `multi-pack-index write
--bitmap`. Repack configuration matches Git's bare/nonbare defaults and last-assignment precedence
across `repack.writeBitmaps` and deprecated `pack.writeBitmaps`, including both source orders.
Bare-repository replacement repacks/gc now write bitmaps by default, adding time and disk cost;
explicit false disables writing. Configured/default resource exhaustion skips optional output
without blocking old-pack retirement; explicit strict caller/standalone writes still return limits.
Other errors after complete pack publication retain that pair's report,
and retirement stops before deletion. Exact replacement sidecars are allowed in both retirement
input-generation comparisons. No-op gc and automatic MIDX writing remain separate policy.

## Multi-pack indexes

The MIDX fixtures in `tests/multi_pack_index.rs` use isolated Git configuration and original pack
workloads. Git 2.56.0 on macOS writes incremental chains with
`git multi-pack-index write --incremental`; it rejects `--split`, although its shipped
`technical/multi-pack-index` design notes still describe that older option. Observed chain layers
have zero base counts and layer-local pack ordinals. Monolithic files take precedence over chains.

The reader checks MIDX v1/v2 structure and checksums in SHA-1 and SHA-256 repositories. Optional
lookup hints retain ordinary pack-index bounds checks and full object identity verification. An
invalid `core.multiPackIndex` boolean disables this optional acceleration. Missing, corrupt, stale
or disabled hints fall back to the pack inventory; packs added after MIDX creation remain visible
when opening a fresh snapshot. Explicit verification checks every claimed ID and offset against the
named pack indexes. It does not replace whole-pack verification. Bitmap queries and remaining MIDX
work remain tracked in [the MIDX issue](https://github.com/joshka/girt/issues/517) and
[the bitmap issue](https://github.com/joshka/girt/issues/518).

With isolated default configuration in an original bare repository, both `git gc --prune=now` and
`git repack -adb` produced a pack bitmap and `.rev` file, and neither created a MIDX. An explicit
`git multi-pack-index write --bitmap` then produced a MIDX and its checksum-named bitmap alongside
the pack bitmap. These are observations of Git 2.56.0, not automatic-maintenance behavior
implemented by girt.

After adding another reachable commit to each fixture, `git repack -ad` and `git gc` removed both
the existing MIDX and its checksum-named bitmap. Girt's retirement follows that invalidation order:
derived MIDX artifacts are removed and their directory synchronized before named packs retire. The
missing-selected-pack regression independently removes a pack named by a valid MIDX and confirms
ordinary reads from a surviving duplicate in both object formats.

A release-build full-read timing on 13 small packs (128 blobs each) used 1664 shuffled IDs, 100
rounds and five alternating repetitions per configuration under `/tmp/girt-timing.lock`. The
disabled/enabled median ratio was 1.0049, which establishes no material speedup. This workload
includes hashing and decoding, whose cost can dominate small-pack full reads; it does not isolate
index-search cost. The run was taken under load (one-minute load 23.74 initially and 22.64 for the
remaining blocks), so absolute timings are not a baseline. Raw block results are in
[`midx-lookup-under-load.json`](evidence/midx-lookup-under-load.json).

Benefits are expected mainly with tens to hundreds of packs, or lookup-heavy workloads containing
many absent IDs, such as existence checks during fetch and negotiation. Current MIDX integration
only selects candidates for verified reads; `Objects::contains` still scans pack indexes. Faster
absence checks remain [follow-up work](https://github.com/joshka/girt/issues/687), not a measured
capability. The `midx_lookup_timing` example provides separate full-read, existence, direct
pack-index scan and direct MIDX search modes for present/absent ID lists. Direct MIDX search omits
candidate validation and outside-pack/loose fallback and must not be presented as end-to-end lookup
performance.

## Submodule definitions

`tests/submodules.rs` generates original repositories through Git with inherited `GIT_*` values
cleared, isolated system/global configuration, and fixed identities and timestamps. Git 2.56.0 on
macOS confirms last-assignment path/URL selection by `submodule init`, name/path separation,
relative-URL interpretation by Git, built-in update copying, traversal-name rejection, invalid
boolean/fetch-recursion refusal, implicit string setting refusal, shadowed invalid assignments,
external-update-command refusal and invalid-ignore fallback. Byte-preservation and conservative
lexical validation have separate library tests. The latter rejects some values Git init accepts; it
does not establish filesystem alias or symlink safety. No checkout, fetch, absorbed-layout, status
or recursion support is claimed by these tests. See
[working with submodule definitions](../site/src/content/docs/guides/submodules.md).

Source tests exercise SHA-1 and SHA-256 worktree/index/commit selection, missing-worktree index
fallback, bare commit reads, byte limits and conflicted/non-regular metadata refusal. URL tests
compare standard local, file, HTTP and scp-like remote shapes with Git init without contacting those
endpoints. Worktree symlinks and relative traversal past the remote path are explicitly refused even
where Git init accepts them. Git refuses inserting a symlink `.gitmodules` index entry; an
independently constructed imported entry verifies girt's refusal too.

`tests/submodule_status.rs` builds original local-file submodules with absorbed Git directories.
SHA-1/SHA-256 cases compare listing prefixes/IDs, inactive and unpopulated checkouts, conflicted
stages, tracked/untracked dirtiness and new commits against Git 2.56.0. Ignore cases compare local
submodule, `.gitmodules` and `diff.ignoreSubmodules` precedence. Global filter fixtures compare
nested Git status with inherited non-local configuration, excluded parent-local settings,
child-local overrides, child-specific conditional includes and explicit command/environment
overrides. Nested untracked content retains its kind through recursion; depth/count limits,
cancellation and symlink-ancestor refusal are separate library checks. Configured status inspects
submodules; raw status remains unchecked. Inspection is macOS/Linux-only and inherits
repository-opening trust assumptions. Standalone listing reads nested metadata even under
`ignore=all`; configured parent status leaves those paths unchecked and unopened. Staged gitlink
changes remain visible. Branch/tag description suffixes are not implemented. Init/update and clone
recursion remain outside this increment.

Captured `XDG_CONFIG_HOME` attributes and ignores reach child status. Regular files replacing
recorded gitlinks produce type changes through the parent's filters; `ignore=all` suppresses those
reads and changes. Git 2.56.0 itself refuses symlink replacements with exit 128, and girt likewise
refuses them. A populated child with an unborn HEAD is a known difference: Git reports it clean and
prints the recorded ID with a space prefix; girt returns a typed configuration error rather than
misrepresenting the child's HEAD. [Issue 688](https://github.com/joshka/girt/issues/688) tracks an
explicit unborn-checkout representation. Standalone listing rechecks the parent index, which
supplies recorded IDs; it does not depend on or recheck the parent HEAD. Configured parent status
retains its index and HEAD rechecks after nested inspection. File/configuration observations are not
atomic.

`tests/submodule_plans.rs` compares proposed init assignments with Git init in both object formats,
including explicit/default update methods, existing URL/update values, false/true activation and
activation pathspecs. Isolated Git invocations execute custom update methods from local, global,
command-line and environment configuration, while rejecting `.gitmodules` commands with exit 128.
Separate tests check library-only acceptance of system/worktree command provenance,
missing-provenance refusal, independent command permission and preservation of the caller's recorded
target. System/worktree command execution has not been compared with Git. Attribute-dependent
activation is explicitly refused. Constructing plans performs no initialization, transport or
checkout. Execution remains tracked in
[issue 507](https://github.com/joshka/girt/issues/507).

`tests/submodule_execute.rs` compares checkout updates with Git in both object formats: detached
HEAD, preserved child branches and target-owned attributes, EOL conversion and filter drivers.
Already matching HEAD retains branch attachment and dirty content. Library checks cover config
lock/stale-input refusal, direct-file byte preservation, caller-selected local config refresh,
parent-local filter exclusion, filter permission/limits, dirty refusal, missing child/commit,
symlink refusal and cancellation. Unit fault injection after checkout verifies retained file/index
effects for late cancellation, parent index changes and failed HEAD publication, preserving the
reference transaction's typed error. Config initialization is portable; checkout is
macOS/Linux-only. Existing absorbed directories and gitfiles are consumed, but creating them and
missing-object fetch are outside this layer. Rebase/merge execution is tracked in
[issue 700](https://github.com/joshka/girt/issues/700); custom update execution is explicitly
refused. No implicit network access, hooks, rollback or recursive checkout occurs. Command-layout
opening retains the caller's cwd/worktree override for fresh checks. One-shot and process-filter
fixtures compare child environment with Git: repository-local context is cleared, child `GIT_DIR`
and empty `GIT_PREFIX` are supplied, and user runtime config parameters/count pairs survive. Child
programs receive the captured snapshot; ordinary pipelines retain their existing ambient-program
environment, tracked separately in [issue 714](https://github.com/joshka/girt/issues/714). A
deterministic lock test confirms stored-HEAD updates with append logging hold the old symbolic
branch lock through checkout.

## Mailmap

`tests/mailmap_git.rs` compares original temporary maps with the installed Git executable's
`check-mailmap`: all four forms, ASCII case folding for names and emails, comments, unknown
identities, duplicate component replacement and name-specific precedence. Repository fixtures
compare worktree/blob/file precedence and the bare `HEAD:.mailmap` default. Oracle helpers clear
inherited Git environment variables and isolate system/global configuration. See the
[mailmap guide](mailmap.md) for presentation policy and configured-path limitations.

## Notes

`tests/notes_git.rs` creates original SHA-1 and SHA-256 repositories with isolated Git configuration
and fixed identities/dates. Add, append, copy and remove compare complete notes commit IDs and tree
IDs, including operation messages and parent links. Nested notes trees generated through girt are
read by both `Notes::read` and `git notes show`, at zero, one, two, three and maximal two-digit
fanout depths. Imported fanout preparation is explicitly refused. See the [notes guide](notes.md)
for literal-byte input policy, bounds and remaining merge/rewrite/layout work.

## Git Capability Audit: Storage

The storage audit uses Git 2.56.0 on macOS and original temporary fixtures. Git oracle helpers clear
inherited `GIT_*` variables and disable system and global configuration. Supported means the stated
operations interoperate; it does not imply identical compression choices or maintenance policy.

| Area                  | Executable comparison                                                                | Result                                                                          |
| --------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| Objects               | `tests/sha256.rs::public_objects_interoperate_both_directions`                       | All four types in both formats; Git checks identities and reads girt writes.    |
| Loose objects         | `tests/blobs.rs::interoperates_with_git_in_both_directions`, `tests/trees.rs`        | Empty, text, binary and 8 MiB blobs; empty trees, all modes and byte names.     |
| Packs                 | `tests/packs.rs::agrees_with_git_for_exact_packed_objects`                           | Both formats, all four kinds, REF and OFS deltas; exact `cat-file` bytes.       |
| SHA-1 and SHA-256     | `tests/sha256.rs`                                                                    | Bidirectional objects, packed history, index, checkout and status.              |
| Writing delta packs   | `tests/packs.rs::git_indexes_and_reads_internal_deltas`                              | Git regenerates the same index, verifies delta depths and reads every object.   |
| Repacking and pruning | `tests/retention.rs::expire_and_prune_match_git_gc_retained_set_and_reflogs`         | Explicit expiry policy matches Git retained objects and reflog bytes.           |
| Alternates            | `tests/alternates.rs::reads_nested_shared_cycles_after_move_without_borrowed_writes` | Relative nested stores, cycles and relocation; borrowed files remain unchanged. |

Repacking and pruning remain partial under [issue 499](https://github.com/joshka/girt/issues/499).
`tests/retention.rs::configured_gc_prune_expiry_requires_caller_policy` compares Git's configured
`gc --no-cruft` with girt's default retention policy for `gc.pruneExpire=now` and `never`, in both
formats. Git removes the old unreachable blob for `now`; girt preserves it until the caller supplies
a cutoff. Pack retirement also deliberately preserves whole recent or protected packs. Partial
clones are refused. Auxiliary indexes and bitmaps remain separate capabilities.

## Git Capability Audit: References

Git 2.56.0 comparisons in `tests/references.rs` cover loose and packed precedence, non-UTF-8 names,
symbolic chains, linked private refs, conditional writers and transactions. The portable reference
tests compare Git writers against girt's locks. `tests/reftable.rs` compares both-format reads,
publication, reflogs and compaction with Git, including case-distinct names on macOS.
`tests/imported_reflogs.rs` and `tests/reflog_policy.rs` compare imported records and automatic
logging policy; `tests/reflog_expiry.rs` compares configured expiry interpretation.

Refs are partial under [issue 550](https://github.com/joshka/girt/issues/550). The new
`cross_worktree_reference_aliases_are_not_representable` comparison demonstrates that Git resolves
`main-worktree/HEAD` and `worktrees/linked/HEAD`, while `RefName` refuses these spellings, in both
object formats. Ordinary shared and current-worktree private refs remain supported. Reftable,
reflogs and conditional precondition checks retain their Supported classification. Files-backed
publication still reports partial I/O effects; this audit does not claim atomic reader visibility
across separate files or worktree stacks.

## Git Capability Audit: Configuration and Revisions

`tests/config_resolution.rs` compares layered values, include order and conditional includes
(`gitdir`, case-insensitive `gitdir`, `onbranch`, and remote-URL `hasconfig`) with Git 2.56.0.
`tests/config_edit.rs` compares edited values with Git and preserves comments, formatting and
multi-valued ordering. `tests/config_continuation.rs` checks continued values. These comparisons
retain the Supported layered-configuration classification.

`tests/revisions.rs` compares single-object expressions with `rev-parse` and range membership with
`rev-list`. [Issue 364](https://github.com/joshka/girt/issues/364) remains the unsupported-syntax
tracker, including dated reflogs, previous checkouts, index paths, message searches, range
shorthands, pseudorefs and describe output. [Issue 550](https://github.com/joshka/girt/issues/550)
adds cross-worktree aliases.

`tests/history.rs::criss_cross_matches_git` compares all best merge bases with `merge-base --all`
and reachable sets with `rev-list`, for loose and packed objects in both formats. `endpoint_queries`
compares ancestry with `merge-base --is-ancestor`, including Git exit codes 0 and 1 and disconnected
histories. Merge bases and ancestry remain Supported. History walks remain partial under
[issue 416](https://github.com/joshka/girt/issues/416):
`root_order_can_differ_from_git_topological_order` demonstrates a parent-first girt walk where Git
`rev-list --topo-order` emits the child first. The current eager walk offers neither date nor
topological ordering; range exclusion can traverse the whole excluded history.

## Git Capability Audit: Working Tree

`tests/index.rs` compares Git-generated index versions, conflict stages, cache-tree, split-index,
resolve-undo and sparse-directory entries. `sparse_pattern_modes_preserve_git_skip_flags` adds cone
and non-cone patterns in both formats: girt preserves Git's skip-worktree flags and leaves excluded
files absent. Index format support remains Supported; applying sparse policy remains partial under
[issue 500](https://github.com/joshka/girt/issues/500).

`tests/status.rs` compares staged/unstaged changes, symlink changes and racy stat data with Git.
`tests/status_ignore.rs` compares per-directory rules, configured sources, parent exclusion and case
folding; `tests/ignore.rs` compares individual glob decisions with `check-ignore`.
`tests/checkout.rs` compares written files, modes, index and subsequent Git operations. Existing
filter, pathspec, monitor-cache and checkout restrictions remain tracked by
[issue 496](https://github.com/joshka/girt/issues/496),
[issue 495](https://github.com/joshka/girt/issues/495),
[issue 519](https://github.com/joshka/girt/issues/519) and
[issue 501](https://github.com/joshka/girt/issues/501).

The raw status comparisons preserve literal index and configuration observation. The
`intent_to_add_status_is_explicitly_refused` comparison records Git's unstaged-addition result
versus raw status's refusal ([issue 576](https://github.com/joshka/girt/issues/576)). Cone and
non-cone fixtures produce clean Git status but are refused by raw status because of skip-worktree
flags ([issue 500](https://github.com/joshka/girt/issues/500)). Existing
`assume_valid_does_not_claim_unverified_content_is_clean` records Git omitting a change that girt
reports ([issue 579](https://github.com/joshka/girt/issues/579)). New configured comparisons show
executable-bit differences under `core.fileMode=false`
([issue 577](https://github.com/joshka/girt/issues/577)) and placeholder symlinks under
`core.symlinks=false` ([issue 578](https://github.com/joshka/girt/issues/578)); the checkout
comparison also verifies Git writing a regular placeholder where raw checkout writes a real symlink.
Configured status applies all four policies; configured checkout writes placeholders. See
[configured index flags and file modes](#configured-index-flags-and-file-modes) and
[configured symlink representation](#configured-symlink-representation).

`case_alias_status_requires_git_policy` records that raw status still compares exact bytes,
reporting a deletion and an untracked case alias where Git with `core.ignoreCase=true` is clean.
Ignore-aware status follows Git ([issue 584](https://github.com/joshka/girt/issues/584)):
`renamed_spellings_match_git_under_name_settings` renames tracked files and directories to other
case and normalization spellings, and `collision_shadows_match_git_under_name_settings` checks out
colliding paths with Git. Both compare every combination of `core.ignoreCase` and
`core.precomposeUnicode` (true, false, unset) with
`git status --porcelain=v1 -z --untracked-files=all`. As in Git, an index path missing from its
directory listing is looked up by its own name. Listed names are composed with precomposeUnicode,
which Git and girt read only on macOS. With ignoreCase, an untracked name is hidden whenever an
index path in its directory matches it ignoring ASCII case. Git does this by name, not by file
identity: on a case-sensitive APFS disk image with `core.ignoreCase=true`, a new file `FILE` beside
a deleted index path `file` is not listed. Girt matches, and the comparison tests ran on such an
image as well as on the default volume. `valid_posix_path_characters_match_git_status` compares
clean and modified Git status for colon and backslash names, including a nested `dir:x/a\b`; girt
matches ([issue 583](https://github.com/joshka/girt/issues/583)). These comparisons distinguish the
raw API's documented policy from configured workflows; they do not authorize changing its existing
contract implicitly.

Two additional comparisons use Git 2.56.0 on macOS.
`existing_user_home_excludes_are_a_git_compatibility_gap` shows that Git expands a real named user's
home for `core.excludesFile` even when `HOME` differs, while girt refuses it
([issue 555](https://github.com/joshka/girt/issues/555)); ignore rules are therefore Partial.
`unicode_worktree_paths_match_git` compares composed, decomposed and nested Unicode names with Git:
raw status is clean and an unchanged checkout changes nothing
([issue 556](https://github.com/joshka/girt/issues/556)).

## Git Capability Audit: Diffs and Blame

`tests/tree_compare.rs` compares structural changes with Git for loose and packed objects, including
mode/type changes and empty sides. Tree diff remains Supported. `tests/rewrites.rs` compares scores,
rename/copy source policies, exact matches, binary candidates and candidate-limit counts with Git.
`configured_rename_limit_requires_caller_options` demonstrates that Git reads `diff.renameLimit`
while girt requires explicit options; rename/copy detection is Partial under
[issue 562](https://github.com/joshka/girt/issues/562).

`tests/content_diff.rs` compares Myers hunks and edit reconstruction with Git, including ambiguous
alignments and NUL classification. `tests/patch.rs` compares unified output. Content diff remains
Partial under [issue 502](https://github.com/joshka/girt/issues/502): patience and histogram are not
selectable. `tests/patch_attributes.rs` compares binary attributes, precedence and function headers,
and verifies textconv refusal. Diff attributes remain Partial under
[issue 462](https://github.com/joshka/girt/issues/462), with additional existing gaps for built-in
driver patterns ([issue 460](https://github.com/joshka/girt/issues/460)) and POSIX pattern semantics
([issue 461](https://github.com/joshka/girt/issues/461)).

`tests/blame.rs` compares line attribution through edits, renames, merges, empty files and shallow
boundaries with Git. `configured_ignore_revisions_are_not_applied` additionally confirms that
`blame.ignoreRevsFile` changes Git attribution but is ignored by girt. Blame remains Partial under
[issue 503](https://github.com/joshka/girt/issues/503), which also tracks moves, copies, textconv
and incremental output. These comparisons use original fixtures and Git 2.56.0.

## Git Capability Audit: Attributes, Tags, Worktrees and Credentials

`tests/patch_attributes.rs::consumer_attributes_and_custom_macros_match_git` compares `text`, `eol`,
`filter`, `merge`, `export-ignore`, `diff`, a custom macro and the built-in binary macro with
`git check-attr`. `named_user_attribute_file_is_explicitly_refused` also demonstrates Git accepting
a named-user home path for `core.attributesFile` where girt refuses it
([issue 555](https://github.com/joshka/girt/issues/555)). Reading these values does not apply their
consumer behavior. Attributes remain Partial: filters are tracked in
[issue 496](https://github.com/joshka/girt/issues/496), merging in
[issue 493](https://github.com/joshka/girt/issues/493), and archive behavior in
[issue 526](https://github.com/joshka/girt/issues/526).

`tests/tags.rs` compares annotated tags targeting all four kinds and imported framing with Git;
`tests/revisions.rs` covers peeling. Tags remain Supported. `tests/worktree_admin.rs`,
`tests/worktree_create.rs`, `tests/worktree_policy.rs` and `tests/worktree_repair.rs` compare Git's
view of created, locked, repaired and pruned registrations, including files and reftable backends.
`existing_branch_add_and_live_removal_are_not_supported` also confirms that orphan registration does
not replace Git’s existing-branch add, and missing-checkout pruning does not replace live removal
([issue 487](https://github.com/joshka/girt/issues/487)). Move remains absent under
[issue 504](https://github.com/joshka/girt/issues/504); cross-worktree reference aliases are
separately tracked in [issue 550](https://github.com/joshka/girt/issues/550).

`tests/layout_shallow.rs` compares shallow walks and ancestry with Git, including refresh after Git
deepens a clone, both formats and linked metadata. Shallow history remains Supported.
`tests/credential_store.rs` compares store/get/erase interoperability and escaped credential fields
with `git credential-store`. `askpass_answers_match_git_credential_fill` compares an original
askpass program with Git's `GIT_ASKPASS` fallback. Credentials remain Supported for explicit helpers
and askpass; applications own interactive terminal prompts. These observations use Git 2.56.0 and
disposable original fixtures.

`tests/http_askpass.rs` runs `git ls-remote` and `workflow::remote_head` against a loopback smart
HTTP server that requires Basic authentication, with recording askpass scripts, and compares the
outcome and every question asked. Cases: `GIT_ASKPASS` before `core.askPass` and `SSH_ASKPASS`,
`core.askPass` before `SSH_ASKPASS`, `SSH_ASKPASS` alone, an empty `GIT_ASKPASS` or `core.askPass`
turning askpass off, an empty answer, a failing program, a configured `credential.username`, and
`credential.useHttpPath`, which adds the repository path to the question. Both ask
`Username for '<scheme>://<host>':` and then `Password for '<scheme>://<user>@<host>': `, dropping
`<user>@` for an empty username. A failing program leaves the question to the next prompt: Git's
terminal, girt's `TransferCallbacks::credential`. Evidence: macOS arm64, Git 2.56.0, 2026-10-18.

`girt-cli/tests/http_remote.rs` lists a configured remote on the same kind of server with
`girt remote heads origin` and `git ls-remote origin`, using one recording askpass script. The
output, including a peeled annotated tag, and the questions asked are identical. The CLI reaches the
remote through `workflow::remote_refs`. Evidence: macOS arm64, Git 2.56.0, 2026-10-18.

## Git Capability Audit: Transfer

The audit baseline is `2e67ff57` (the merged Git guide), with original fixtures observed using Git
2.56.0 on macOS and a case-insensitive temporary filesystem. Storage through transfer retain the
scope of their named comparisons; these are not claims of exhaustive Git CLI equivalence.

`tests/fetch_workflow.rs` compares refspec mapping, exclusions, conditional publication and pruning
with Git fetch. `automatic_tags_and_fetch_head_remain_caller_owned` confirms that Git's default
fetch follows a reachable annotated tag and writes `FETCH_HEAD`, while girt leaves both absent.
Fetch/clone remain Partial under [issue 505](https://github.com/joshka/girt/issues/505): automatic
tags, `FETCH_HEAD` and multi-round HTTP negotiation remain caller work or absent. HTTP currently
sends all haves in one request. `tests/clone.rs` compares repository layouts, selected HEAD,
configuration, object connectivity and checkout behavior.

`git_partial_clone_filter_has_no_girt_fetch_equivalent` creates a Git blob-filtered bare clone and
observes its missing blob without lazy fetching, then confirms that the current girt fetch installs
the complete blob. Filters and promisor fulfillment remain absent under
[issue 521](https://github.com/joshka/girt/issues/521). Server implementation remains separate under
[issue 520](https://github.com/joshka/girt/issues/520); using Git's server as an oracle does not
implement a girt server.

`tests/push.rs`, `tests/push_deltas.rs`, `tests/http.rs` and `tests/ssh.rs` compare accepted and
rejected refs, partial success, leases, delete commands, thin deltas and Git readability. Push
retains Supported. HTTP and SSH tests exercise v0/v1/v2 discovery and fetch, depth and incremental
receipt. `tests/ssh.rs::protocol_v2_fetch_agrees_with_git_client` additionally runs Git's client and
girt through the same independently generated SSH fixture in both object formats, comparing commit
bytes and checking received connectivity. Local, HTTP and SSH client transports retain Supported;
HTTP/SSH remain feature-gated, and SSH requires the documented native platform support.

The all-feature macOS transfer run passed its non-SSH suites, but four of 133 SSH cases failed with
service exit 1 under host contention. Focused deletion reruns passed; protocol reruns had mixed
results before a diagnostic rerun passed all five cases, and the direct Git-client comparison passed
both cases. [Issue 572](https://github.com/joshka/girt/issues/572) records this unclassified
validation failure. It does not establish a missing protocol capability. Follow-up serial results
and classification will be recorded in that issue; fixture processes were absent after the completed
runs.

## Commit-Graph Metadata

`commit_graph::CommitGraph` reads standalone SHA-1 and SHA-256 graph files with ordered parent
edges, including octopus merges, generation v1 and corrected commit dates (GDA2/GDO2). Parsing
checks the checksum, chunk boundaries, fanout, sorted identities and parent positions under explicit
byte, commit and parent bounds. Each layer additionally limits decoded parents to twice its commit
count plus its encoded extra-edge count, bounding expansion from shared edge runs. This is
structural validation, not object-store verification. Unknown chunks are ignored. Split chains
resolve global parent positions against checked base identities and reject missing, reordered or
duplicate layers. Repository loading honors `core.commitGraph`; malformed indexes return errors so
callers can use objects instead. Explicit `CommitGraph::verify` checks trees, ordered parents and
committer dates against object reads. It does not validate generation equations. Callers can
explicitly attach a graph to `Objects` to reuse metadata for walks and merge bases. Attachment
checks closed, acyclic ancestry; range and ancestry pruning additionally require a strict
corrected-date or topological-generation order. Unknown coverage or generation order uses full
traversal. Indexed queries preserve shallow boundaries and commit/parent bounds; per-object read
limits apply to fallback reads. Repository object readers attach valid graphs by default, honoring
`core.commitGraph=false`, and fall back to object reads for missing, corrupt or structurally
unusable graphs. Explicit `CommitGraph::verify` checks object claims before querying;
`Objects::without_commit_graph` removes the index from a reader. Supported Bloom filters can skip
tree comparisons in first-parent path walks and single-parent blame steps; unknown filters fall back
to tree reads.

`tests/commit_graph.rs` generates original histories using the installed Git executable, writes both
generation versions with `git commit-graph write --reachable`, runs `git commit-graph verify`, and
compares metadata and octopus parent order with the original objects. It also checks every truncated
prefix, checksum corruption and resource limits. Unit fixtures independently encode the format
documented by `git help gitformat-commit-graph`; checksum-preserving malformed chunk, fanout and
parent records must fail. The suite is portable and selected for Windows validation.

The Bloom hash implementation follows independent Git CLI observations. The installed Git 2.56.0
`git help gitformat-commit-graph` manual prints the second Murmur seed as `0x7e646e2`; Git-generated
filter bytes for `notes` and `other` instead match `0x7e646e2c`. Tests exercise both filter
versions, UTF-8 and raw high-byte paths, long paths and every directory prefix, empty and nonempty
roots, first-parent merge differences and a 600-path change producing the conservative all-ones
filter. Path history is compared with `git log --first-parent --format=%H -- path`. No upstream
implementation or test source is used.

Graph construction reads verified commit objects and calculates generations independently of
attached auxiliary indexes. Standalone and incremental encoders emit both object formats, octopus
edges, corrected-date overflow and base-file positional references. Git CLI verification accepts
original girt-written standalone and chained fixtures. New layers omit Bloom filters; construction
refuses shallow history and unrepresentable timestamps. Encoding returns artifacts; locked
publication separately installs standalone files or split chains. Tests verify migration, held-lock
preservation, failed manifest replacement with a readable previous index, and unchanged object
integrity through Git fsck. Incremental construction currently verifies the full selected object
closure.

Git 2.56.0 CLI fixtures show `git gc` writing a standalone graph by default and
`gc.writeCommitGraph=false` suppressing it. Default `git maintenance run` and explicit
`--task=commit-graph` produce split chains. An isolated `GIT_TRACE2_EVENT` observation confirms
`commit-graph write --split --reachable --no-progress` during default maintenance. girt exposes
explicit publication independently of maintenance scheduling; it does not yet automatically write
graphs during retention/GC.

The [commit-graph timing evidence](evidence/commit-graph.md) records quiet-window G/A/B/B/A/G blocks
on 82,500 commits. The initial indexed process medians were 111 ms for a 351-commit range and 114 ms
for ancestry, versus Git's 16 ms and 10.5 ms. Profiling identified repeated ancestry and generation
parent lookups. Prefix-narrowed lookups and combined structural validation reduce indexed process
medians to 28.7 ms and 27.0 ms in a before/after window, versus Git's 16.1 ms and 9.54 ms (1.78 and
2.83 times Git). Opening still dominates at approximately 19–20 ms; indexed query phases take 1.46
ms and 0.074 ms. Raw wall-time, RSS, phase and per-slot load measurements accompany both
comparisons. This does not establish Git parity.

## Imported Object Decoding and Tag Peeling

R07 reads historical tree modes, legacy commit metadata/framing, and tags with unreadable tagger
metadata in both object formats. Graph consumers use the leading tree/parent or tag target records
without demanding construction-ready identities. `Commit::author`, `Commit::committer`, and
`Tag::tagger` expose absence separately from identity errors; `IdentityRef::date` reports date
interpretation failures while retaining name, email and exact date bytes. `to_fields` performs an
explicit fallible conversion to editable construction fields. Canonical constructors remain separate
from import acceptance.

`Objects::peel` retains the original tip and ordered tag identities, verifies declared target kinds,
and returns a terminal identity/kind within tag, byte and per-read limits. Contextual errors retain
the original tip, failing identity, referring tag and depth. Fetch update validation uses the same
operation. Transfer edge discovery follows readable trees in stored order; tree comparison and
checkout still require their own name/order validation. Pure decoding and peeling do not publish
objects or references. The [R07 evidence](evidence/r07.md) records executable-Git differences
between reading, display, canonical writing and strict fsck, plus signature-provider payload
captures.

## R06 Native Validation Snapshot

The [R06 native validation](evidence/r06.md) records this platform boundary at revision
`f79b50843a47aeed7b468acb7b4a133f35e98a10`:

| Platform       | R06 evidence boundary                                    |
| -------------- | -------------------------------------------------------- |
| macOS arm64    | Full suite through raw checkout, including HTTP and SSH. |
| Linux x86_64   | Full suite through raw checkout, including HTTP and SSH. |
| Windows x86_64 | Portable integration suites and bounded HTTP runtime.    |

Its run IDs, counts, environments and Windows exclusions apply to that revision. Later validation
appears in the next section.

## Platform and Git-Version Validation

### R36 Coverage Inventory

[R36 evidence](evidence/r36.md) inventories the current Windows selection and repairs the R08/R10
home-condition and discovery failures. The existing platform workflow adds reference and HTTP
integrations; portable fetch planning, clone completion, enumeration and tracing units no longer
inherit Unix-only fixture gates. The final
[broad native run](https://github.com/joshka/girt/actions/runs/36128800142) passes all four hosts at
`f0bed7e87022282f72db1cea40da763e318ad23f`. Windows passes 1,397 all-feature units, 743 core
integrations, 44 HTTP cases and 20 doctests, plus separate core-only and tracing selections. Exact
commands, environments, focused runs and remaining exclusions are recorded in the evidence. R11/R12
retain their focused ref/index evidence. C02 remains the broader storage/layout gate; Windows
status/checkout refusals and unsupported local/SSH transport do not establish parity.

The completed [R12 broad run](https://github.com/joshka/girt/actions/runs/36126493241) at
`0e42acf744437732604355e719e70a2eae3ead99` passes all three Unix jobs and fails the nine known
Windows cases. It is retained as the R36 baseline, not a green Windows milestone.

### Final Roadmap Validation

[Run 36089083146](https://github.com/joshka/girt/actions/runs/36089083146) passed all four native
jobs on 2026-09-25 UTC at code/workflow revision `cf8b49b3dcaaa7335ebb6454278e9db5d7638bbb`,
published on `joshka/roadmap-validation`. It includes content diff, index read/write, raw status,
raw checkout, and the checkout preparation fix rejecting planned nested-repository markers before
worktree mutation.

| Native runner       | Units | Integration | Doctests | Total |
| ------------------- | ----- | ----------- | -------- | ----- |
| Ubuntu 22.04 x86_64 | 1236  | 517         | 19       | 1772  |
| Ubuntu 24.04 x86_64 | 1236  | 517         | 19       | 1772  |
| macOS 14 arm64      | 1236  | 515         | 19       | 1770  |
| Windows 2022 x86_64 | 941   | 244         | 19       | 1204  |

Unix runtime suites enable all features. Windows units/doctests enable all features; integrations
comprise 237 core-only cases across the deliberate portable selection plus seven HTTP-only cases.
The new Windows selection executes 16 content-diff cases, 17 index cases, two status boundary cases
and two checkout boundary cases. The latter verify cancellation and unsupported-platform refusal,
not Windows status traversal or checkout materialization. All selected tests passed without ignored
or filtered cases. The [suite inventory](testing.md#native-platform-coverage) retains the
operation-specific exclusions.

All runners independently passed core-only, HTTP-only and SSH-only library compilation and
all-feature/all-target Clippy with warnings rejected. Unix private Rustdoc passed with warnings
rejected. The content-diff and index examples ran on every host; raw status and checkout examples
ran on Unix alongside local fetch/push and HTTP/SSH examples. All hosts used Rust 1.98.1; Git was
2.55.0 on Unix and 2.55.0.windows.5 on Windows/MSVC. Compilation and examples are additional
evidence, separate from test counts. Local `just check` passed after the final fixes with 1,770
tests, formatting, Clippy and docs.rs. The four new examples, Windows GNU core-library cross-Clippy
and workflow `actionlint` also passed locally; the cross-check is compilation evidence only.

[The initial checkpoint](https://github.com/joshka/girt/actions/runs/36088621269), revision
`db183b8aa1cb9988e356229c4f80d185b161886e`, exposed two validation failures. Ubuntu 22.04 failed an
assertion expecting a directory mutation to change timestamps during a short scan. The fixture now
sets an old directory mtime first, making the observed metadata change independent of clock tick
resolution. Windows Clippy rejected a cancellation helper returning the large checkout error by
value. The unsupported-platform path now selects its cancellation/refusal cause directly; the helper
is compiled only for the native checkout implementation. Public errors and cancellation precedence
are unchanged. Both previously failing stages pass in the final native run.

Raw status/checkout remain macOS/Linux-only and preserve literal bytes and POSIX modes without
attributes, filters, EOL conversion, ignores or Git configuration normalization. macOS requires
ASCII names in enumerated directories; Linux preserves supported byte filenames. Checkout retains
its conservative path, gitlink, repository-marker and index-extension refusals, caller exclusion of
concurrent writers, partial-operation reporting and lack of rollback/crash durability. This run does
not expand those contracts. Windows refs/reflogs, composed fetch/clone publication, local/SSH
process adapters and actual raw status/checkout remain unsupported. Windows HTTP evidence remains
the bounded plaintext suite; HTTPS/trust and the full Unix transport fault matrix remain untested.

This result record is a later Markdown-only local child, validated with rumdl and markdownlint-cli2;
it is not the CI-tested revision. Historical results below remain tied to their stated revisions.

### Portable Integration Validation

[Run 36078287817](https://github.com/joshka/girt/actions/runs/36078287817) passed all four jobs on
2026-09-25 UTC at revision `86ba2618b4d3ec598428d24a7b075ba3c205608e`, published on
`joshka/portable-platform-validation`. This revision includes repository initialization/discovery,
reference enumeration/deletion, reflog transactions, remote/refspec mapping, fetch orchestration,
clone, and recursive tree comparison. Native results are:

| Native runner       | Units | Integration | Doctests | Total |
| ------------------- | ----- | ----------- | -------- | ----- |
| Ubuntu 22.04 x86_64 | 960   | 441         | 16       | 1417  |
| Ubuntu 24.04 x86_64 | 960   | 441         | 16       | 1417  |
| macOS 14 arm64      | 960   | 439         | 16       | 1415  |
| Windows 2022 x86_64 | 793   | 207         | 16       | 1016  |

Unix runtime suites enable all features. Windows units and doctests enable all features, while
integration coverage consists of 200 core-only tests across nine explicitly selected suites and
seven HTTP-only tests. The [suite inventory](testing.md#native-platform-coverage) records operations
and exclusions. Windows now exercises objects and loose storage, pack/index reading and writing,
history queries, structural tree comparison, repository opening/initialization/discovery, and
remote/refspec mapping against Git. The Linux-only filesystem-path and ref-name cases account for
the two additional Linux tests.

Windows HTTP runtime evidence covers real Git download, owned-worker validation, object installation
and known-object reuse; pushing objects with Git publishing the remote ref; rejection of
authentication failures, server errors and redirects without retries; truncated RPC bodies; and
stalled-discovery deadlines. These fixtures use disposable loopback plaintext HTTP and a bounded Git
CGI subprocess. Windows HTTPS/trust validation, upload cancellation, uncertain push reports and the
broader Unix fault matrix remain untested. Reference storage, reflog transactions and composed
fetch/clone publication remain unsupported on Windows, as do the owned local-process and SSH
adapters. Git's own ref operations in a fixture do not establish girt ref support.

Every runner passed isolated core-only, HTTP-only and SSH-only library compilation and
all-feature/all-target Clippy with warnings rejected. Windows SSH feature compilation does not
provide an SSH adapter. Unix runners also passed warning-denying private Rustdoc and local fetch,
local push, HTTP and SSH examples. All hosts used Rust 1.98.1; Git was 2.55.0 on Unix and
2.55.0.windows.5 on Windows with the MSVC toolchain. These compilation checks are separate from the
runtime counts above; no performance, crash-durability or network-filesystem evidence was added.

[The preceding run](https://github.com/joshka/girt/actions/runs/36077535098), revision
`9be3b78a6aa122a23d8cb0d19194ceca35350e0b`, passed all Unix jobs but found a Windows fixture
assertion comparing Git's `C:/...` spelling to Rust's canonical `\\?\C:\...` spelling.
Canonicalizing Git's reported path fixed the assertion without changing library behavior. The final
Windows run executed all selected suites with no ignored or filtered tests. Portable tests now use
`--no-fail-fast`, and HTTP runs independently after other step failures so its result remains
visible. An earlier superseded run was cancelled after local Clippy identified needless borrows in
the new fixture; no result is claimed for it.

Local `just check` passed on the initial portable-suite revision (1,415 tests); the path correction
then passed its focused regression, all-target/all-feature Clippy and formatting. `actionlint`,
rumdl and markdownlint-cli2 also passed. This result record is a later Markdown-only child, not a
CI-tested revision. Earlier capability sections retain their original revision-specific evidence;
this native run supplies the later platform results.

### Architecture Revision Validation

[Run 36043376324](https://github.com/joshka/girt/actions/runs/36043376324) passed on 2026-09-24 at
revision `c948ec24c56cdb6ac585f32b8a3fcc655f1c9846`, after the architecture fixes.

| Native runner       | Runtime results                               |
| ------------------- | --------------------------------------------- |
| Ubuntu 22.04 x86_64 | 685 units, 316 integration tests, 12 doctests |
| Ubuntu 24.04 x86_64 | 685 units, 316 integration tests, 12 doctests |
| macOS 14 arm64      | 685 units, 314 integration tests, 12 doctests |
| Windows 2022 x86_64 | 609 portable units, 12 doctests               |

All runners passed core-only, HTTP-only, and SSH-only library compilation and all-target Clippy. All
runners used Rust 1.98.1. Unix runners used Git 2.55.0; Windows used Git 2.55.0.windows.5 with the
MSVC toolchain. Unix suites enabled all features and included real loopback HTTP/HTTPS and SSH
interoperability, owned network handoff, transport interruption, incremental transfers, and delta
compression. Local fetch, local push, and HTTP examples passed. Clippy enabled all features and
rejected warnings; Unix private Rustdoc also rejected warnings. Windows runtime commands used
default features, so HTTP compilation is not HTTP runtime evidence.

Windows reference storage and local/SSH process adapters remain unsupported. These portable tests do
not establish Windows repository integration, HTTP runtime compatibility, crash durability,
network-filesystem behavior, or hostile-path safety. Isolated feature checks compile libraries; they
do not execute independent feature-specific runtime suites.

Local `just check` also passed on the published implementation, including formatting, 1,011 tests,
all-feature/all-target Clippy, and docs.rs. Earlier capability sections retain their original
revision-specific limitations; this run supplies later platform evidence without changing those
historical results.

[Run 36043806070](https://github.com/joshka/girt/actions/runs/36043806070) also passed all four jobs
at revision `e26c36345a340292cb5fe04c74e2e09e484f556e`, with unchanged library and test code. This
workflow revision gives each isolated feature check its own step so a later PowerShell command
cannot hide an earlier failure, and adds the disposable SSH example on all Unix runners. Test counts
and toolchains match the preceding run; all four Unix examples now pass on each Unix runner.
`actionlint` passed for the workflow. This evidence document is a later Markdown-only change,
checked with rumdl and markdownlint-cli2; the CI results above identify the tested code exactly.

### Historical Cross-Platform Runs

The repository capabilities preceding transport interruption have runtime evidence on macOS arm64
and Linux x86_64. The [owned transport change](#owned-transport-interruption) records its evidence
separately. Windows reference storage remains unsupported. Earlier capability sections below retain
their original baseline environments; this section records the later cross-platform validation.

| Target                    | Runtime evidence                 | Boundary                 |
| ------------------------- | -------------------------------- | ------------------------ |
| macOS arm64               | Full suite and transfer examples | Trusted local filesystem |
| Ubuntu 22.04/24.04 x86_64 | Full suite and transfer examples | Trusted local filesystem |
| Windows 2022 x86_64       | 537 units, 11 doctests           | References unsupported   |

Native checks used macOS 26.6.2 arm64 and rustc 1.98.1 with Git 2.55.0 and Apple Git 2.54.0.
Commands were `just check`, `PATH=/usr/bin:$PATH cargo test --locked`,
`RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --document-private-items`, and
`cargo run --locked --example fetch_local` / `cargo run --locked --example push_local`. The final
native suite contains 788 tests, including doctests. The final malformed-config regression also
passed separately with Apple Git using
`PATH=/usr/bin:$PATH cargo test --locked --test repositories malformed_syntax_is_rejected_by_git_and_girt`.

[GitHub Actions run 35944395649](https://github.com/joshka/girt/actions/runs/35944395649) passed at
revision `ca01ec99e01bc6c257c76e26876b90a7f25e04d4`. Both Ubuntu hosts ran 790 tests, including the
Linux-only non-UTF-8 path/ref cases; macOS 14 arm64 ran 788. All three passed all-target Clippy,
private Rustdoc and disposable local fetch/push examples. All used rustc 1.98.1 and Git 2.55.0.
Windows 2022 MSVC passed all-target compilation with rustc 1.98.1 and Git 2.55.0.windows.5.
[Run 35944525644](https://github.com/joshka/girt/actions/runs/35944525644), revision
`92856caf9dc8bc4e15480a291b2da58a938b456a`, also passed 537 Windows library-unit tests and 11
doctests using `cargo +stable test --locked --lib` and `cargo +stable test --locked --doc`. An
unused Unix-only test import warning was subsequently fixed; the Windows job now also rejects
all-target Clippy warnings. These checks do not establish Windows repository support: reference
storage explicitly rejects non-Unix platforms, and several integration tests require it. Local
server process/environment and filesystem semantics still need Windows integration coverage before
expanding that contract.

[Platform CI](../.github/workflows/validation.yml) records toolchain and Git versions and repeats
those checks. Native Linux VM attempts on the development Mac failed to boot/connect, so the Linux
runtime evidence comes from CI. Library cross-checks also passed using
`cargo check --locked --lib --target <target>` for `x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-gnu`, and `x86_64-pc-windows-msvc`. Windows all-target cross-checking on the Mac
was blocked by missing MinGW for Criterion's `alloca` dependency; the native Windows CI compile
succeeded. Cross-compilation alone does not establish runtime behavior. Crash durability, network
filesystems, hostile path mutation and multi-gigabyte workloads remain outside this evidence.

Git 2.54.0 and 2.55.0 were exercised, not a minimum supported version. Fixture commands rely on
`init --object-format=sha1`, `--initial-branch`, `mktag --no-strict`, pack/index commands and local
upload-pack/receive-pack v0. CI's installed Git versions must be retained with results; runner
labels alone do not establish compatibility with an older Git release.

The independent review traced object framing/identity and pack/delta bounds, ref locks and
publication, repository/config interpretation, ancestry queries, and transfer validation/status
paths. It found and fixed two config parser discrepancies: leading unquoted whitespace after empty
quotes or a continued line was retained, and a backslash followed by CR CR LF was incorrectly
treated as a continuation. New unit and Git-comparison cases cover both; the unit cases failed
before the fix. Existing corruption, concurrency, namespace, graph and partial-push tests were
rerun. This is a correctness review within the documented capabilities, not proof of complete Git
fsck equivalence or an exhaustive security audit. No performance claim or processing-path redesign
was introduced.

## References and Provenance

The format references are [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects)
and [Git's hash transition document](https://git-scm.com/docs/hash-function-transition). Blob
identity hashes the uncompressed header and content. Loose files contain a zlib stream, located
under the first two hexadecimal identity digits and a filename containing the remaining digits.

Implementation and tests are original. No Git source, test code, or comments were copied or adapted.
`tests/blobs.rs` generates empty, text, binary, and malformed inputs at runtime. Its fixed
`ABC_LOOSE` byte array was independently generated with Python's `zlib.compress(b"blob 3\0abc")`. A
complete-read case validates that fixture; named cases test all 18 incomplete prefixes. No fixture
files are vendored.

Run `cargo test` with Git available on `PATH`. Interoperability tests create isolated bare SHA-1
repositories using `git init --bare --object-format=sha1 --template=`. They compare identities with
`git hash-object --stdin`, read girt-written objects with `git cat-file blob`, then remove those
loose files and regenerate them using `git hash-object -w --stdin` for girt to read. Tests remove
inherited `GIT_*` environment overrides and disable system/global config for child processes. They
do not modify global configuration or the working checkout. This slice was validated with Git 2.55.0
on macOS; other platforms have not been exercised.

## Independent Unit-Test Identities

- `src/object.rs` checks exact headers at 9/10-byte and 99/100-byte decimal transitions, using
  repeated `x` bytes. A fifth case preserves every byte value from 0 through 255, including NUL and
  non-UTF-8 bytes.
- Each named case compares the complete encoding with a literal header plus the original payload,
  and the identity with a literal expected SHA-1 value. Expected values are not computed by girt.
- The constants were established with Python 3.14.7 `hashlib` and independently cross-checked with
  Git 2.55.0. The empty-blob identity remains covered by the existing doctest.
- Reproduce the constants outside a repository with this script; all temporary state is disposable:

```python
import hashlib
import os
import subprocess
import tempfile

fixtures = [b"x" * size for size in (9, 10, 99, 100)] + [bytes(range(256))]
env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
with tempfile.TemporaryDirectory() as directory:
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.path.join(directory, "absent"))
    for content in fixtures:
        encoded = b"blob " + str(len(content)).encode("ascii") + b"\0" + content
        expected = hashlib.sha1(encoded).hexdigest()
        result = subprocess.run(
            ["git", "hash-object", "--stdin"], input=content, cwd=directory, env=env,
            check=True, capture_output=True,
        )
        assert result.stdout.decode().strip() == expected
        print(len(content), expected)
```

## Validation Boundaries

Tests cover empty and binary content, exact read limits, missing objects, unsupported formats and
object types, malformed headers and lengths, wrong identities, every truncation of a small
compressed object, invalid checksums, trailing bytes, and concatenated streams. Publication tests
cover repeated and concurrent writes, existing corrupt objects, and failed publication with
temporary-file cleanup.

Reads require canonical decimal lengths and complete zlib streams. They reject malformed encodings
rather than emulate Git's tolerance for unusual inputs. Compressed bytes can differ from Git's
output; identity and decoded bytes must agree. This slice does not implement collision detection,
packed objects, alternates, power-loss durability, shared-repository permissions, or protection
against a hostile filesystem owner. Files are published with a rename that refuses to replace an
existing path (`renameat2` on Linux, `renamex_np` on macOS), or with a hard link and unlink where
that rename is unavailable. New files use zlib's best-speed level, Git's default for loose objects
when `core.looseCompression` and `core.compression` are unset; girt does not read those settings.

Performance commands, cache conditions, results, and limitations are recorded in the
[blob benchmark baseline](benchmarks.md).

## Dependencies

Runtime dependencies are `sha1` for hashing, `flate2` for zlib, `tempfile` for exclusive temporary
files and disposable test directories, and `thiserror` for error derivation. All four declare
`MIT OR Apache-2.0`. Requirements permit compatible releases within their selected release series;
the lockfile records tested versions.

`rstest` provides test parameterization and Criterion provides benchmark sampling and analysis. Both
are development-only dependencies with MIT/Apache-2.0 license alternatives.

The resolved dependency manifests were checked with `cargo metadata --format-version 1`. Resolved
packages offer MIT, Apache-2.0, or Zlib terms, including platform-specific dependencies;
`unicode-ident` also requires Unicode-3.0 terms. `simd-adler32` and `generic-array` use MIT;
`zlib-rs` uses Zlib. License alternatives in transitive packages do not require selecting LGPL. This
records package metadata, not an independent legal audit.

## In-Memory SHA-1 Trees

`Tree` supports the five standard entry modes: regular blob (`100644`), executable blob (`100755`),
symlink (`120000`), subtree (`40000`), and gitlink (`160000`). Payload names are byte strings and
IDs are 20 raw SHA-1 bytes. `Tree::new` rejects empty names, NUL, slash, dot, dot-dot, and
byte-identical duplicates, then sorts entries in Git order. Directories compare as though their name
ends in `/`; other entries, including gitlinks, compare with a NUL terminator.

`Tree::parse` preserves entry order, duplicate names, and invalid path components. It accepts only
the exact mode spellings above; zero-padded and historical permission modes are explicitly
unsupported. Accepted payloads re-encode byte-for-byte, and identity hashes those unchanged bytes
with the canonical tree object header. `Tree::validate` checks names, duplicates, and ordering
without modifying the object. It is not a complete `git fsck` check: `.git`, platform aliases,
all-zero IDs, referenced object existence, and referenced object types are not checked.

The references are [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects),
[`git mktree`](https://git-scm.com/docs/git-mktree), and the diagnostic categories in
[`git fsck`](https://git-scm.com/docs/git-fsck). Ordering and encoded bytes are independently
checked through Git commands, without consulting or adapting Git implementation code.

`src/tree.rs` tests literal encodings for every supported mode, empty-tree identity, byte names,
file/directory prefixes, nonadjacent duplicate file/tree names, invalid names, malformed and
unsupported modes, missing delimiters, and truncated IDs. `tests/trees.rs` generates original
fixtures at runtime, including whitespace and non-UTF-8 names. No fixture files are vendored.

Both-direction interoperability tests run in disposable bare SHA-1 repositories with inherited
`GIT_*` overrides removed and system/global configuration disabled. `git mktree -z --missing`
constructs trees from independently specified, unsorted listings; `git cat-file tree` supplies
Git-produced payloads for girt to parse and compare byte-for-byte. The tests then remove the Git
object and publish girt's tree with `LooseObjects::write_tree`. `git ls-tree -z` and `git mktree`
reconstruct the same identity. Empty and mixed-mode trees use this complete sequence. Synthetic
referenced IDs intentionally avoid requiring commits or reference resolution in girt. Additional
tests compare preserved unsorted, duplicate-name, and invalid-name identities with
`git hash-object --literally`.

Validated with Git 2.55.0 on macOS arm64. Other platforms have not been exercised. The tree API
performs no filesystem operations and makes no checkout-safety guarantee. It allocates owned entries
and encoded buffers proportional to input size; callers bound input size before parsing. There is no
separate resource-limit, partial-write, cleanup, or concurrency contract in this in-memory slice.
SHA-256 trees, filesystem traversal, the index, checkout, commits, references, and packs remain
outside scope. No dependencies were added.

## Loose SHA-1 Trees

`LooseObjects::read_tree` verifies the canonical object header, payload size, complete zlib stream,
and requested SHA-1 identity before parsing. `write_tree` stores the exact encoded payload,
including supported unsorted entries, duplicate names, and invalid names. Neither operation calls
`Tree::validate` or resolves references. Unparseable payloads return `Error::Tree` with the
underlying cause; wrong object types return `Error::UnsupportedObjectType`.

`tests/trees.rs` reads actual Git-written loose files and compares their identities, entries, and
payloads. After deleting those files, it publishes trees through girt and checks Git's `cat-file`,
`ls-tree`, and `mktree` results. Noncanonical fixtures use
`git hash-object -w --literally -t tree --stdin`; girt reads and republishes their exact bytes,
which Git reads back unchanged. All fixtures are original runtime data in isolated repositories.

Unit tests in `src/loose.rs` cover empty and populated trees, exact and exceeded payload limits,
highly compressed oversized input, malformed framing, identity mismatch, tree parse failures, wrong
types, missing storage, duplicates, concurrent writes, corrupt existing files, and cleanup after
failed publication. The existing blob suite continues to cover every truncated prefix of a zlib
fixture, checksum damage, trailing data, and concatenated streams through the shared decoder.

The executable `examples/loose_tree.rs` writes two blobs, constructs and stores a tree referencing
them, then reads and checks the tree and both blobs. The size limit bounds decompressed payload, not
total heap use: parsing additionally owns entries and names proportional to the payload. Storage
shares blob publication and filesystem assumptions, including the no-replace publication requirement
and no power-loss durability guarantee. Validated with Git 2.55.0 on macOS arm64; other platforms
remain untested. No dependencies were added.

## SHA-1 Commits and Loose Storage

`Commit` supports a tree ID, ordered parent IDs (including root and merge commits), author and
committer byte identities, signed 64-bit Unix seconds, timezone offsets, arbitrary message bytes,
and opaque extra headers with multiline values. Parsing requires the standard order: tree, parents,
author, committer, extra headers, then a blank line. IDs must be 40 hexadecimal SHA-1 digits;
offsets must be signed four-digit `HHMM` with hours below 24 and minutes below 60. Missing,
reordered, or repeated required headers and continuations on required headers are unsupported.
Malformed framing, truncated headers, SHA-256 IDs, and dates outside this grammar return errors. A
shortened message is still a valid payload; only loose framing/identity can detect that corruption.

Parsing retains the entire payload and decoded fields. Encoding and hashing preserve uppercase IDs,
signed or padded seconds, `-0000`, unknown/repeated extra headers, continuation spaces, non-UTF-8
bytes, and messages without a final newline. Continuation values remove exactly one framing space
per line. Construction writes canonical IDs and dates and validates nonempty identity components
without NUL, CR, LF, angle brackets, or surrounding ASCII whitespace. New seconds span the full
signed `i64` range, including pre-epoch dates. Extra-header names must be printable non-space ASCII
and cannot reuse required keys; new values reject NUL and CR. `validate` applies these field rules
without modifying parsed bytes. Explicit reconstruction may change identity by normalizing lexical
details.

`CommitPayload` borrows structurally framed headers and message bytes without requiring decoded
identities, dates, IDs, or required-header ordering. Header occurrences retain their exact folded
values; `without_headers` copies every byte except explicitly selected complete header spans. Both
`gpgsig` spellings and repeated headers can be selected independently. `Signature::parse` optionally
interprets identity bytes using the first opening and following closing email delimiter, trimming
framing whitespace before the opening delimiter and before seconds. Raw bytes remain available when
interpretation or construction policy fails. See [R01 evidence](evidence/r01.md) for independent Git
observations, signature selection semantics, and the concrete R07 decoding follow-ups.

Construction is not full `git fsck`: messages can contain NUL (which Git's default `hash-object`
rejects), parent IDs can repeat or be zero, and referenced objects are not checked. Encoding and
signature headers are opaque; no charset conversion, embedded-tag parsing, or cryptographic
verification occurs. SHA-256, history traversal, refs, packs, and transport remain outside this
slice.

The format references are [`git commit-tree`](https://git-scm.com/docs/git-commit-tree) for commit
metadata and date offsets, and [`gitformat-signature`](https://git-scm.com/docs/gitformat-signature)
for multiline extra headers. Implementation and fixtures are original, based on those descriptions
and observed Git behavior; no Git source code or tests were consulted or adapted.

`src/commit.rs` covers literal encoding, a fixed independently calculated SHA-1 identity, root and
ordered merge parents, message and header byte preservation, lexical reconstruction, malformed
framing, invalid identity components, reserved keys, date bounds, and overflow. The fixed identity
uses Python `hashlib.sha1(b"commit " + str(len(payload)).encode() + b"\0" + payload)` and is
cross-checked with Git in `tests/commits.rs`.

Integration tests isolate bare SHA-1 repositories, remove inherited `GIT_*` overrides, disable
system/global configuration and templates, and set explicit author/committer identities and dates.
`git commit-tree` independently creates root, single-parent, and merge commits. Tests compare
fields, complete payloads, and identities, remove Git's loose files, publish girt's constructed
commits, and check `cat-file` and strict `fsck` against real trees and parents. Additional tests
check girt-created binary messages and unknown multiline headers in both storage directions;
`hash-object --literally` permits NUL and noncanonical fixtures. These tests preserve bytes without
claiming that Git considers those fixtures valid. No fixture files or new dependencies are added.

`LooseObjects::read_commit` verifies framing, size, zlib completion, and identity before parsing;
`write_commit` publishes exact bytes without calling `validate`. Commit storage unit tests cover
exact/exceeded payload limits, oversized compressed data, wrong type, malformed framing, identity
mismatch, parser errors with retained causes, missing storage, duplicate/concurrent writes, existing
corruption, and publication cleanup. The shared blob decoder tests cover truncated zlib streams,
invalid checksums, concatenated streams, and trailing compressed bytes. Size limits bound
decompressed payload, not total heap use: parsed commits also own decoded fields and retained bytes.
Publication inherits the documented no-replace publication and trusted-filesystem assumptions and
does not promise power-loss durability.

`examples/loose_commit.rs` exercises blob-to-tree-to-commit storage and reads the referenced
snapshot back in disposable storage. Validated with Git 2.55.0 on macOS 26.6.2 arm64; other
platforms are untested. The [commit baseline](benchmarks.md#commit-baseline) records performance
evidence.

## SHA-1 Annotated Tags and Loose Storage

`Tag` supports a target ID and declared `ObjectKind` (blob, tree, commit, or tag), byte name,
optional `Signature` tagger, opaque extra header lines, and arbitrary message bytes. Creating or
storing an object does not create a tag reference, resolve the target, check its actual type, or
peel nested tags. Embedded signature armor remains part of the message without verification.

The format reference is [`git mktag`](https://git-scm.com/docs/git-mktag). Independent command-line
probes and `tests/tags.rs` verify tags with absent taggers, opaque extra lines, and empty messages
without a blank separator. Git's strict `mktag` promotes missing-tagger diagnostics to errors and
rejects extra headers; those fixtures explicitly set `fsck.missingTaggerEntry=ignore` and
`fsck.extraHeaderEntry=ignore`. This documents preservation rather than a claim that every supported
payload passes default strict validation.

Parsing requires ordered `object`, `type`, and `tag` headers, followed by an optional `tagger` and
extra lines. Headers end in LF; a blank line introduces arbitrary message bytes. Without a blank
separator, every header must still end in LF and the message is empty. Known headers cannot repeat
or appear among extra lines. Targets require exactly 40 hexadecimal digits; unknown types, SHA-256
IDs, truncated headers, and unreadable tagger dates fail explicitly. Taggers reuse the commit
identity/date grammar. Parsing retains lexical details and does not apply construction validation.
Extra lines retain their full bytes and order, including bare keys, repeated unknown keys, and
leading spaces; they are not decoded as commit continuations.

Construction rejects empty names, ASCII whitespace or NUL in names, and extra lines containing NUL,
CR, or LF. Taggers apply the existing identity/date validation. Absent taggers are allowed. This
does not validate tag reference paths or promise full fsck validity. Construction emits a blank
separator even for empty messages, lowercase IDs, and canonical dates; reconstructing parsed fields
can change identity. Encoding, hashing, and storage use the retained payload unchanged.

Original runtime fixtures in `tests/tags.rs` use isolated bare SHA-1 repositories with inherited Git
overrides removed and global/system configuration disabled. `git tag -a` independently creates tags
of each of the four object types. Tests compare exact fields, payloads, and identities, remove the
Git-written loose object, publish through girt, and check `cat-file`, `mktag`, and strict `fsck`.
Additional fixtures check absent taggers, omitted separators, unknown lines, and opaque signature
text through relaxed `mktag`. Noncanonical dates, uppercase IDs, binary content, and invalid
construction fields use `hash-object --literally`; they promise byte preservation only. No Git
implementation code or fixture content was copied or adapted.

The literal unit identity `1316e0263ce4c0bc7afc85d20286be352811d4cf` was independently obtained with
Git for a tag of the empty blob named `v1`, tagger `A <a> 1 +0000`, and no message separator. Unit
tests also exercise all target types, framing errors, unsupported formats, byte names, reserved
headers, opaque PGP/SSH signature text, and validation failures without losing original bytes.
Storage tests cover exact limits, oversized decompression, wrong types, missing files, corruption,
parser errors after identity verification, duplicate/concurrent publication, and cleanup while
preserving existing corrupt files. Existing shared decoder tests cover zlib truncation and trailing
data. The example `loose_tag` demonstrates storage without tag refs.

Validated with Git 2.55.0 on macOS arm64; other platforms are untested. No dependencies were added.
Storage retains the existing trusted-directory, no-replace publication, synchronous I/O, and
durability limits. The read limit bounds decompressed payload bytes, not the additional owned fields
and payload copies. SHA-256, tag refs, peeling/traversal, signature verification, discovery, packs,
and transport remain outside scope. [Performance evidence](benchmarks.md#tag-baseline) records the
tag baseline.

## Repository Opening and Configuration

`Repository::open` accepts exactly a worktree root, Git directory, or indirection file. Ordinary,
bare, separate Git directories and linked worktrees are covered. It returns canonical Git/common
paths, the shared object directory, and the associated worktree. No upward search, initialization,
reference resolution, packed-object reads, graph traversal or transport is performed.

The original implementation follows the published
[repository layout](https://git-scm.com/docs/gitrepository-layout),
[configuration syntax](https://git-scm.com/docs/git-config#_syntax) and
[repository version](https://git-scm.com/docs/repository-version) specifications. Independent
runtime fixtures in `tests/repositories.rs` use Git 2.55.0 on macOS arm64. They create repositories
with `git init --template=`, separate metadata with `--separate-git-dir`, and linked worktrees with
`git worktree add`. Git-generated loose blobs are read through the opened repository. No dependency
was added, and no Git implementation or upstream fixture was copied.

### Configuration Boundary

- `Config::parse` consumes bytes without I/O. `Config::resolve` reads explicit system/global/local/
  worktree sources, environment pairs and caller overrides, preserving ordered occurrences and
  source provenance. See [layered configuration](configuration.md) for inputs, limits and refresh.
- Section/variable lookup folds ASCII case while retaining original spelling. Quoted subsections
  preserve bytes and case; deprecated dotted subsections fold ASCII case. Repeated, implicit and
  empty values remain distinct. Scalar lookup selects the last occurrence.
- Parsing supports comments, mixed quoted/unquoted values, escapes, physical continuations, LF/CRLF
  and an initial UTF-8 BOM. Values need not be UTF-8. NUL, invalid escapes, multiline quoted
  strings, unsectioned variables and malformed headers fail with a physical line number.
- Repository format bootstrap reads only direct common configuration. Versions 0/1 with SHA-1 and
  version 1 with SHA-256 are supported. Unknown formats/extensions, refStorage, and objectFormat
  under version 0 are rejected. Version-0 extension rejection is stricter than historical Git.
- Direct `extensions.worktreeConfig` enables the private `config.worktree` source. Its direct core
  settings participate in layout detection; format settings, includes and inherited sources cannot
  reinterpret the object format. Without the extension, the file is ignored.
- Core integer settings support signed decimal/octal/hexadecimal and binary k/m/g suffixes. Boolean
  words ignore ASCII case; implicit values mean true and empty/numeric zero mean false.
- Linked layouts accept absolute/relative backlinks and preserve inaccessible registrations. Shared
  core.worktree is ignored without worktreeConfig; with it, direct common/private settings select
  the checkout. See [repository layouts and shallow history](repositories.md).
- Relative core.worktree paths resolve against the Git directory. Bare/worktree conflicts and empty
  worktree values fail. Tilde and `%(...)` path interpolation is unsupported. A directly supplied,
  non-bare metadata directory can open with an unknown checkout location, distinguished from bare by
  `Repository::is_bare`; no current-directory worktree is guessed. Unix path conversion preserves
  bytes. A non-UTF-8 filename fixture is Linux-only because the exercised macOS filesystem rejects
  such filenames; byte conversion itself is tested on macOS. Other platforms require UTF-8 metadata
  paths and remain untested.

### Failure and Compatibility Evidence

Named Git comparisons cover comments, whitespace, escaping, continuation, CRLF/BOM, byte values,
Git-written quoted subsections, repeated keys, boolean forms, numeric versions and relative
core.worktree. Repository fixtures cover root, Git-directory and gitfile inputs, shared linked
objects, both object formats, unsupported sources/extensions/storage, malformed metadata, missing
paths, and no parent discovery. The runnable example is exercised with hostile Git environment and
global configuration settings in its child process; those settings do not affect opening.

Opening checks HEAD's marker shape without resolving its reference and requires object and refs
directories. It reads format-aware shallow snapshots and permits local alternates metadata. History
follows declared shallow boundaries without changing raw commit parents. The repository opener does
not inspect packs. `Repository::objects` opens bounded pack snapshots; `Repository::loose_objects`
continues to search only loose storage.

File-content snapshots before and after successful and rejected opening establish absence of file
creation, deletion or content changes. Filesystem access times are not covered by that guarantee.
Missing repositories, malformed metadata, configuration syntax failures, filesystem errors and
unsupported features have distinct error variants with source paths. Opening performs synchronous
reads and allocations without a configurable resource limit. Concurrent metadata replacement is not
a consistent snapshot, and canonicalization is not an ownership or security check. Symlinks are
resolved; callers must supply a trusted repository path and handle later storage changes.

## Files References

### Contract and Supported Boundary

`Repository::references` provides a borrowed files-backend store. `RefName` owns exact bytes and
accepts full `refs/…` names following Git's name rules, plus `HEAD`; it does not normalize or expand
shorthand. Other pseudorefs (including multi-record FETCH_HEAD), revision expressions,
cross-worktree aliases such as `main-worktree/HEAD`, and reftable are excluded. Repository opening
continues to reject every refStorage extension, including an explicit `files` value; repositories
using the implicit default files backend are supported. Unix storage is enabled, with macOS arm64
exercised; non-Unix storage returns an explicit unsupported error.

Loose refs take precedence over packed refs. A malformed loose value reports an error even when a
valid packed value exists. Direct targets are nonzero SHA-1 IDs; symbolic records begin with `ref:`
and a supported full name. Trailing ASCII whitespace is accepted, and writes produce one LF. The
reader rejects zero IDs, SHA-256 IDs, invalid symbolic names and extra non-whitespace data. It
supports regular files, rejecting filesystem symlinks (including legacy symlink HEAD) and other
non-regular files. Parent symlinks are also rejected. Names remain bytes through parsing and Unix
path conversion, but filesystem restrictions still apply: APFS rejects non-UTF-8 loose filenames.
Packed byte-name reads are tested on macOS; loose byte-name writes have a Linux-only test that has
not been exercised here. Case/Unicode aliases inherit filesystem behavior; no normalization or
portable alias detection is provided.

`HEAD`, `refs/bisect/`, `refs/rewritten/`, and `refs/worktree/` are local to the opened worktree;
other names and packed-refs use its common directory. Resolution returns the terminal name and an
optional ID. Missing initial refs and unborn/dangling symbolic chains return an absent ID; missing
objects are not detected. Cycles are distinct errors, and callers set the maximum symbolic hops for
reads. Updates through symbolic chains allow at most 32 hops. Resolution follows live reads rather
than a snapshot and never peels tags or loads objects.

### Packed Grammar

An empty or absent packed-refs file is accepted. Nonempty files use LF-terminated records,
optionally starting with `# pack-refs with:` and space-separated `peeled`, `fully-peeled`, and/or
`sorted` traits. Unknown traits are unsupported. Each direct record contains 40 hexadecimal digits,
one space and a validated shared reference name. A single `^` record with a nonzero 40-digit ID may
immediately follow a direct record. Peeled IDs are checked syntactically and omitted from lookup
results; the traits do not prove object type, existence, or correctness of the peel. Resolution
returns the direct tag-object ID.

Both headerless/unsorted records and Git-generated sorted/peeled files are supported. Claimed sorted
order is checked bytewise. Duplicate names, misplaced headers, orphan/repeated peel lines, blank
lines, unknown comments, invalid names/IDs, missing final LF and packed per-worktree names fail.
Whenever packed fallback is needed, the entire file is read and validated, including unrelated
records; a loose hit does not open it. Every update, deletion, and enumeration validates
packed-refs. Reads allocate in proportion to file size without a configurable limit. There is no
packed cache or indexing yet.

### Conditional Publication and Reflogs

`update_without_reflog` replaces the named reference itself, including a symbolic reference.
`update_resolved_without_reflog` locks and preserves each symbolic hop and writes only its terminal
name. `Expected::Absent` requires no loose or packed value; `Value` compares the stored target;
`Any` accepts any valid value or absence. Conditions are checked under locks. For resolved updates,
the terminal value is direct or absent, so an expected symbolic value cannot match. Symbolic writes
may create dangling names or cycles, but symbolic HEAD must point into `refs/`; resolution reports
those states separately.

Every write acquires common `packed-refs.lock` by exclusive creation, followed by the destination
lock (or all visited symbolic-chain locks). Holding the packed lock stabilizes packed expectations
and namespace checks against cooperating Git writers and serializes even unrelated girt updates.
Existing locks cause an immediate error, with no retry or lock stealing. Ancestor/descendant
conflicts in packed refs are checked before publication; filesystem conflicts reject loose
namespaces, including empty directories. A successful write renames its complete owned lock over the
loose destination. This shadows an existing packed value without changing packed-refs or unrelated
reference data. Conditional deletion removes packed data first, as described below.

These methods deliberately omit all reflog writes regardless of `core.logAllRefUpdates` and leave
existing logs unchanged. Git normally creates/appends relevant logs and records old/new IDs and
identity information. girt's API names make opting out visible at the call site. Prior tips gain no
new reflog-based retention or recovery record, old unreachable objects may become eligible for
pruning, and history shown by `git reflog show` omits girt updates. The test confirms that Git's
`main@{0}` can still return the current tip despite the unchanged log; this selector alone cannot
verify logging. No hooks run, and there is no object existence/type or fast-forward check. This is
low-level reference publication, not full Git update-ref behavior or a checkout operation.

The filesystem must provide exclusive file creation and atomic rename; callers must use a trusted
repository and cooperating lock-protocol writers. Configuration, layout, symlink and lock ownership
must not be changed adversarially during an operation. Reads across refs/hops are not transactional.
Failures before rename preserve old values, but empty directories may remain. Owned locks are
removed on ordinary errors/unwind; cleanup I/O failures or process termination can leave stale
locks. No fsync is issued, so successful visibility does not guarantee crash/power-loss durability
for either refs or objects. Network filesystems and crash recovery have not been validated.

### Packing References

`References::pack` moves loose direct references into `packed-refs`, like `git pack-refs`.
`PackScope::All` matches `--all`; `PackScope::Tags` matches the default, which packs only
`refs/tags/`. Output is byte-identical to Git 2.56 for SHA-1 and SHA-256: the
`# pack-refs with: peeled fully-peeled sorted` header (with its trailing space), records in name
byte order, and a `^` line with the fully peeled ID after every annotated tag, including nested
tags. Existing packed records are kept, including those whose objects are missing, and an existing
`^` line is kept when its objects can no longer be read, as Git keeps it. Symbolic refs, `HEAD`,
per-worktree names, malformed loose files and refs to missing objects stay loose, as with Git.

The whole operation holds `packed-refs.lock`. The replacement is synced, renamed, and its directory
synced before any loose file is removed. Each removal takes that ref's lock without waiting and
unlinks only if the file still holds the packed value. A locked or changed file stays and is
reported, and it still shadows its packed record. A crash at any point therefore leaves every ref
either loose or packed with its old value. If the directory sync fails after the rename,
`PackRefsError::Unconfirmed` reports the visible replacement and nothing is pruned. On non-Unix
hosts `packed-refs` is still written, but no loose file is removed and every packed name is reported
as retained. This follows the deferral of destructive Windows maintenance in
[the roadmap](jj-roadmap.md#b08--nonadmin-windows-destructive-maintenance): without a nonadmin
durability barrier for the rename, the loose copy stays as the durable one. Clone behaves the same
way. Concurrent Git writers that delete refs need `packed-refs.lock` and cannot interleave. Reftable
is unsupported.

`PackTrigger::Auto` approximates `git pack-refs --auto` as observed with Git 2.56. It packs once at
least 16 loose candidates exist, and the threshold grows by 5 for each doubling of `packed-refs`
past 12 KiB. Git's exact heuristic is not reproduced. Clone packs every fetched ref before
publishing the local branch and HEAD, leaving the same loose files as `git clone`.

`workflow::fetch` then runs the pack-refs task of Git's automatic maintenance, which `git fetch`
runs afterwards as `git pack-refs --all --prune --auto`. `Repository::auto_pack_references` exposes
the same task for consumers to run after their own batches of reference writes, as Git porcelain
such as `git commit` does. As observed with Git 2.56, the task runs when `maintenance.auto` is true
or, if unset, `gc.auto` is positive, and when `maintenance.pack-refs.enabled` is true or, if unset,
`maintenance.strategy` is unset or `geometric`. Invalid or unknown values, including the strategy
`none` that Git 2.56 rejects, disable it, as they make Git's maintenance fail. A pack failure, such
as a held `packed-refs.lock`, does not fail the fetch, matching `git fetch`'s exit status. An
invalid `maintenance.auto` or `gc.auto` makes `git fetch` exit 128 after updating its references;
girt's fetch still succeeds. Unlike Git, the task runs in the foreground, not detached.
`tests/packed_refs.rs` and `tests/clone.rs` compare the bytes and the remaining loose files with
Git's, and check round trips where Git reads, updates, deletes and repacks girt's output.
`src/transfer_pack_tests.rs` compares each fetch's layout with `git fetch` under the same
configuration.

### Evidence and Provenance

Original `tests/references.rs` fixtures invoke Git 2.55.0 in isolated temporary repositories with
ambient Git configuration and environment removed. Git `update-ref`, `symbolic-ref`, annotated
`tag`, `pack-refs --all`, and `worktree add --detach` produce input. Git `check-ref-format`,
`rev-parse`, `show-ref`, `symbolic-ref` and `reflog show` check results. No Git source or upstream
test fixtures were used, and no dependencies were added. References consulted were
[`git-check-ref-format`](https://git-scm.com/docs/git-check-ref-format),
[`gitrepository-layout`](https://git-scm.com/docs/gitrepository-layout),
[`git-pack-refs`](https://git-scm.com/docs/git-pack-refs) and
[`git-update-ref`](https://git-scm.com/docs/git-update-ref).

Tests cover both loose and packed input, annotated-tag peel records, loose shadowing without packed
mutation, byte names, detached/unborn HEAD, dangling symbolic and object targets, exact conditional
updates, and symbolic replacement versus terminal updates. Linked worktree tests compare each
private namespace and shared branches with Git. Rejection/failure tests cover name rules, malformed
records, unsupported backends, cycles/depth, namespace conflicts, zero IDs, symlinks, existing
locks, failed conditions, write/rename failure, owned-lock cleanup, and preservation of unrelated
data. Two-writer races exercise girt/girt and girt/Git conditional updates; exactly one writer
succeeds. These cover cooperating writers, not arbitrary direct file rewrites or crash consistency.

The runnable `publish_branch` example stores two commits, publishes an unborn branch through HEAD,
and conditionally advances it while preserving HEAD's symbolic value. Criterion measures warm loose
reads, HEAD resolution, no-reflog updates, and packed lookups over 10 and 10,000 refs; see the
[reference baseline](benchmarks.md#reference-baseline). Reflogs and multi-ref transactions remain
outside reference storage. Later increments add enumeration/deletion and the other repository
capabilities documented separately below.

### Reference Enumeration and Deletion

`References::list` returns owned `Reference { name, target }` values in bytewise name order. It
includes all `refs/` namespaces visible to the opened worktree, with shared refs from the common
directory and private refs from the current worktree. HEAD and other pseudorefs are excluded. Use
`list_namespace` with `refs/heads` or `refs/tags` to select branches or tags; a namespace selects
the exact name and its slash-delimited descendants. Missing namespaces are empty. Symbolic targets
are returned as stored, including dangling targets and cycles; tags are not peeled. Loose values
shadow packed values. Invalid loose data fails instead of falling back, and conflicting effective
names are rejected. Empty directories, dot-prefixed entries and `.lock` entries are ignored.

Enumeration validates all packed data once and visits selected loose paths without locking. It
allocates for the packed data, effective names and pending directories. There is no configurable
size limit or point-in-time snapshot. Concurrent additions/deletions may be missed or included; a
vanished loose ref can leave a previously read packed value in the result. An owned list stays
unchanged after return, but callers must pass an expected value when acting on its observations.
Malformed selected loose files and filesystem symlinks are errors; unselected loose files are not
read. Packed errors anywhere fail even a filtered enumeration.

`delete_without_reflog` compares and deletes the named stored target. A symbolic name is removed
without following it. `delete_resolved_without_reflog` locks up to 32 symbolic hops and deletes only
the terminal name; HEAD can remain unborn and aliases can remain dangling. Both require an explicit
`Expected`: `Value` checks the effective current value, `Absent` accepts only absence, and `Any`
accepts a valid value or absence. Deleting an absent name with `Absent` or `Any` succeeds without
changing reference contents. Conditions and existing data are checked before any reference mutation.

Deletion holds `packed-refs.lock` and the selected name's lock (or the entire symbolic chain). It
stages a replacement packed file in the common directory and atomically publishes it while retaining
the packed lock, then unlinks the loose file. Removing the packed record first prevents loose
removal from exposing an older packed value. Only the selected record and its following peel line
are removed: unrelated bytes, header spacing/traits, ordering, hexadecimal case and peel records are
retained. Loose-only deletion does not create packed storage. Parent directories, reflogs and
objects are left in place. No hooks or reflog writes occur.

This is not a two-file transaction or crash recovery protocol. A precondition failure preserves both
reference files. Packed publication failure leaves both values intact. If packed removal succeeds
and loose removal fails, `ReferenceError::PackedDeleted` reports partial completion: the current
loose ref remains while its packed copy has been removed. No rollback is attempted. Locks and
temporary files have best-effort cleanup; empty directories or stale cleanup files may remain. No
fsync is performed. Cooperating writers are excluded while the locks are held, but readers holding
old packed data can observe stale values, and later writers can recreate a deleted name. The
Unix/local/trusted-filesystem assumptions above remain in force.

Original unit and integration fixtures cover namespace boundaries and ordering, loose/symbolic
shadowing, malformed input, exact expectations and absence, symbolic-name versus terminal deletion,
cycle/depth failures, lock contention, temporary publication failure and partial unlink failure.
Git-created branches and annotated tags exercise packed removal with unrelated peel preservation;
Git `show-ref`, `rev-parse`, `symbolic-ref` and `for-each-ref` observe the results. Linked worktrees
exercise all three private namespaces alongside shared packed branches, and a separate Git directory
exercises gitfile routing. Races with Git conditional updates and `pack-refs --all` exercise writer
coordination; a retained-lock experiment confirms that Git packing refuses the reserved packed file
after independent replacement. These tests validate observable behavior without Git source or
upstream fixtures. They do not establish arbitrary crash recovery or reader snapshot isolation. The
`publish_branch` example now enumerates and conditionally deletes the branch it published.

## Pack Reading

`Repository::objects_with(PackLimits)` returns a synchronous reader with live loose-object access
and a pinned set of pack/index files. `Objects::read(id, ReadLimits)` returns a verified kind and
exact payload, or `None` for absence. Blob, tree, commit, and tag payloads share this boundary;
structured parsing remains separate. Existing loose typed reads and writes retain their contracts. A
corrupt loose copy fails without falling through to a valid packed duplicate.

### Formats and Validation

SHA-1/SHA-256 pack v2/v3 and index v1/v2 are supported, including index large-offset tables,
ordinary objects, OFS_DELTA and REF_DELTA. Unknown versions return explicit errors. Legacy index v1
has no CRC table. Reserved object types fail when read. Index binary search uses strictly ordered
full IDs; an offset-ordered table identifies entry boundaries and OFS_DELTA bases.

Opening pins all pairs within caller-supplied aggregate artifact-size and pack-count limits. It
validates index lengths and checksums, sorted unique IDs, exact fanout counts, large-offset
references, unique in-range entry offsets, pack headers/counts and index/pack trailer agreement. It
reads only pack headers and trailers. Reads check available entry CRC32 values while inflating;
`Objects::verify_packs` explicitly streams whole packs to check their checksums and entry CRCs. An
index without its pack is an I/O error. Unindexed packs are ignored: indexes are publication markers
(see [fetch installation](#upload-pack-fetch)). Entry framing, exact zlib termination, declared
lengths and delta programs are checked on demand, including traversed bases and intermediate
results; reads verify the requested object's decoded identity. Opening alone does not certify
payload syntax or all object identities. The checks detect corruption; SHA-1 collision detection is
not provided.

REF_DELTA resolves only inside its own pack, including forward references. An external or missing
base returns `MissingBase`, even if another pack or loose storage contains that identity. Iterative
traversal detects cycles and bounds depth. Reads bound each object, each delta program, and the sum
of all inflated and reconstructed bytes. Limits are charged before decoding/allocation. These bounds
exclude allocator overhead, index tables, fixed decompression scratch space, and structured parsing
performed by callers; they are not a total process-memory quota.

Pack/index files remain open for the reader's lifetime; index identities stay on disk and bounded
offset tables stay in memory. Pack bytes and decoded objects are not cached. Files must remain
immutable while pinned; in-place mutation is outside the consistency contract. Opening is not an
atomic topology/pair snapshot. [R16](#r16-object-store-refresh) defines refresh and publication.
Loose reads remain live. Trusted paths and ancestors are required; this API does not secure hostile
concurrent filesystem mutation. [R15](#r15-file-backed-storage) records resource and cancellation
policy. Multi-pack indexes and bitmap/reverse indexes are ignored. Local alternate stores and
complete partial-clone stores are supported without network retrieval;
[R39 evidence](evidence/r39.md) records their bounds and topology contract. Thin packs still require
transport import; raw stored-pack reads do not resolve external bases.

### Independent Evidence

The implementation uses the public
[Git pack format specification](https://git-scm.com/docs/gitformat-pack), not Git source code.
`src/pack/tests.rs` constructs original binary fixtures for malformed/truncated inputs, checksums,
CRCs, offsets, ordering, fanout, large-offset tables, reserved kinds, delta instructions, forward
references, nested deltas, missing/external bases, cycles, exact limits, and resource failures.
Fixtures are constructed in memory; no downloaded binary fixtures are used.

`tests/support/pack_git.rs` generates isolated SHA-1 bare repositories using Git `hash-object`,
`mktree`, `commit-tree`, and `mktag`. `pack-objects --stdout --no-reuse-delta --no-reuse-object`
with and without `--delta-base-offset` produces independent OFS_DELTA and REF_DELTA packs.
`index-pack --index-version=2` builds indexes. `verify-pack -v` proves that actual delta entries
exist, and their reported offsets let the fixture assert the intended encoding. After
`prune-packed`, `tests/packs.rs` compares all object IDs, kinds, and exact payload bytes with Git
`cat-file`. It also covers mixed storage, loose corruption precedence, absent objects, orphaned
files, loading limits, and snapshot reads after `repack -ad` deletes the original pair.

The exercised environment is Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64 (Apple M2 Max). Other
operating systems and actual multi-gigabyte packs have not been exercised; small fixtures cover the
64-bit offset representation. The new direct dependency `crc32fast` uses `MIT OR Apache-2.0`
(checked in the resolved 1.5.2 package metadata); it was already transitive via flate2. A
[Criterion baseline](benchmarks.md#pack-read-baseline) records opening, indexed misses, ordinary
reads, and delta reconstruction. `examples/packed_repository.rs` is the runnable consumer.

## Commit Message Cleanup

`tests/message_git.rs` runs an original message corpus through Git 2.56.0 with isolated
configuration. `git stripspace` and `git stripspace -s` match `Cleanup::Whitespace` and
`Cleanup::Strip` byte for byte with the `#`, `//` and `;` comment prefixes, including carriage
returns, form feeds, NUL bytes and non-ASCII whitespace. `git commit -e -F` with
`commit.cleanup=scissors` stores what `Cleanup::Scissors` returns. Unit tests cover the
`commit.cleanup` and `core.commentChar`/`core.commentString` configuration rules observed with the
Git CLI: case-sensitive mode names, `default` and `scissors` acting as `whitespace` for unedited
messages, and the last of the two comment variables winning.

`CommentPrefix::auto` picks the first of `#;@!$%^&|:` that starts no line and returns `None` when
all do, where Git refuses to choose. Git 2.56.0's commit command drops trailing `#` comment lines
from a `-F` message before choosing (`x\n;z\n#z\n` gets `#`, while `x\n#z\n;z\n` gets `@`); callers
reproducing `git commit -F` drop those lines first. Git deprecates `auto`. Every assignment of
`core.commentChar` and `core.commentString` is validated, not just the last.

## Commit Message Trailers

`tests/message_git.rs` runs 49 original messages through `git interpret-trailers --parse` and
through eight `--trailer` argument sets under 16 configurations, comparing every output byte for
byte with `girt::message::trailers` and `add_trailers` (Git 2.56.0, isolated configuration). The
corpus covers block detection (the first paragraph, comment lines, continuation lines, the 25% rule
with `Signed-off-by:` lines (followed by a space), cherry-pick notes and configured keys, URLs,
scissors lines and `---` dividers), separators, `trailer.where`, `trailer.ifExists` and
`trailer.ifMissing` globally and per key, configured key abbreviations, and how Git rewrites the
existing block.

Rules learned from the Git CLI that its documentation does not state: a key matches a configured
section when, without trailing `-`, it abbreviates the section name or key; a new trailer matches an
existing one when the shorter key is a case-insensitive prefix of the longer; continuation lines do
not count toward the 25% rule; and with no trailer block Git appends a blank line even when it adds
no trailers. `trailer.<name>.command` and `.cmd` are refused with `TrailerCommandError` because girt
does not run trailer commands; `--trim-empty`, `--if-exists` and the other per-invocation option
overrides have no equivalent yet.

Git 2.56 changed one rule: earlier versions read a `key://…` line, such as a URL, as a trailer
(`git interpret-trailers --parse` prints `http: //example.com`), while 2.56 does not. girt follows
2.56. A unit test pins that behaviour, and the live comparison skips the URL case when the Git under
test is older, as on CI runners with Git 2.55.

## Reset

`tests/reset_git.rs` builds one original repository (three commits, a side branch, staged, unstaged,
added and untracked changes), copies it, runs `git reset` (Git 2.56.0, isolated configuration) in
one copy and `Repository::reset` or `reset_paths` in the other, then compares HEAD, the branch HEAD
names, `git ls-files --stage`, `git status --porcelain`, the last two entries of HEAD's and the
branch's reflogs, `ORIG_HEAD` and the operation files in the Git directory. Cases: soft and mixed
resets, a reset to the current commit (Git logs it in HEAD's reflog only), a detached HEAD, commit
resets removing `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `MERGE_HEAD`, `MERGE_MODE`, `MERGE_MSG`,
`SQUASH_MSG` and `AUTO_MERGE`, a refused soft reset during a merge, path resets from HEAD and from
another commit, `git restore --staged --source` with a glob pathspec, a path reset that resolves
conflict stages, a path reset where a file becomes a directory (Git removes the file's entry even
though the pathspec does not select it), path resets in a repository without commits, and
assume-unchanged flags, compared with `git ls-files -v` (kept on unchanged entries, cleared where
the content changes). A soft reset with conflict stages in the index and no `MERGE_HEAD` is refused,
as Git refuses it. A mixed reset keeps the cached stat data of unchanged entries.

Deliberate differences: girt locks HEAD's branch before writing anything, so a locked branch fails
with nothing written, where Git has already published `ORIG_HEAD` and the index; and a failure to
write `ORIG_HEAD` stops the reset before anything else changes, where Git warns and continues.
Commit resets refuse reftable repositories for now, because Git keeps `ORIG_HEAD` and `AUTO_MERGE`
in the reference table there.

Observed with the Git CLI and matched: every commit reset, including one to the current commit,
writes `ORIG_HEAD` and a `reset: moving to <revision>` reflog entry; path resets write neither and
leave operation files alone; rebase and sequencer directories are not touched. girt refuses indexes
with skip-worktree entries or sparse directories instead of resetting them partially; keep and merge
resets are not implemented yet.

Hard resets are compared the same way, adding every working file's content and executable bit:
discarding staged, unstaged and added changes, restoring a deleted file, keeping untracked files,
replacing an untracked file where the target has a file or needs a directory, keeping a clean file
whose attributes change while its blob does not, a hard reset on a branch with no commits
(`Repository::reset_unborn`), and resetting out of a conflicted merge to a commit with a mode
change. Hard resets go through checkout's discard policy, so they share its path, symlink and
platform rules (Linux and macOS). Two deliberate differences: Git deletes an untracked directory,
with its contents, where the target has a file, while girt refuses before changing any working file;
and girt leaves directories emptied by removed files in place, as its checkout does, where Git
removes them.

## Describe

`tests/describe_git.rs` generates four original 50-commit histories with merges and nondecreasing
commit dates, tags about a sixth of the commits (annotated with repeated tagger dates, or
lightweight, some sharing a commit) and adds branches, a remote-tracking branch and a notes ref. For
every other commit it runs `git describe --long --abbrev=40` under twelve option sets (default,
`--tags`, `--all`, `--match`, repeated `--match`, `--exclude`, `--all --match`, `--candidates` of 0,
1 and 2, and `--first-parent` with and without `--tags`), with and without a commit-graph, using Git
2.56.0 with isolated configuration. Both tools find a name for the same commits, girt's distance
always equals `git rev-list --count` from the commit to its chosen reference, and wherever Git's
printed distance is exact the two outputs are byte-identical. `girt-cli/tests/git_parity.rs`
compares `girt history describe` with `git describe` for default, `--tags`, `--long --abbrev=12` and
`--all`.

Git's distance is an estimate made while walking newest commit date first. On two of the four
histories it differed from the exact count 18 and 9 times, printing a larger number or preferring a
reference whose estimate was smaller although another was nearer (for example `notes/x-8` where
`tags/rel-3` is 6 commits away). girt reports the exact count and the nearest reference among the
same candidates. Tie-breaking rules observed with the Git CLI and matched: among equally near
candidates the newer one wins; at one commit annotated tags beat lightweight tags, which beat other
references, the newest tagger date wins, then the alphabetically first name. `--match` and
`--exclude` are case-sensitive globs whose `*` crosses `/`; with `--all` they apply to names under
`refs/tags/`, `refs/heads/` and `refs/remotes/` and exclude every other reference. A symbolic
reference names its target's commit under its own name, as in Git, and tags of trees and blobs are
skipped. An annotated tag whose reference was renamed is printed under the tag object's name and
always in the long form (with the full commit ID under `--abbrev=0`), while patterns match the
reference name; `tests/describe_git.rs` compares these cases and a recent tag on a 400-commit
history found within a 20-commit bound. `--contains` is not implemented.

`tests/revisions.rs` compares revision parsing of `git describe` output (`<name>-<count>-g<hex>`)
with `git rev-parse --verify`: the name is not looked up but must be a valid branch name, the count
must be decimal, the hex needs at least four digits (a full ID may carry trailing zeros), a
reference with the whole name wins, an ambiguous prefix resolves to its one commit even when a blob
shares it, and steps such as `~1` and `^{tree}` may follow.

`girt history describe --dirty` and `--broken` are compared with `git describe` in
`girt-cli/tests/git_parity.rs` for a clean tree, untracked files only, a modified file, a staged
file, a damaged index, a deleted intent-to-add entry, an untracked file in a submodule and a failing
required clean filter on a resized file; as in Git, only tracked and staged changes count.

## Commit History

`tests/support/history_git.rs` generates original commit payloads and stores them through Git
`hash-object`, with alternating timestamps that disagree with ancestry. `tests/history.rs` compares
reachable sets with `git rev-list`, ancestry with `git merge-base --is-ancestor`, and best common
ancestors with `git merge-base --all`. Criss-cross graphs verify two bases in loose and packed
storage; endpoint cases include identity and disconnection. Git 2.55.0 on macOS arm64 was exercised.

Walk ordering is a girt contract, not a reproduction of Git CLI ordering: breadth-first first
encounter, supplied root order, stored parent order, without duplicates. Commit parsing and identity
verification reuse the existing APIs. Missing parents are errors, including when an endpoint already
answers the query. Referenced trees are not resolved. Annotated tags must be peeled by the caller.
Declared shallow boundaries stop parent traversal; raw commit parents remain available. See
[shallow snapshots](repositories.md#shallow-snapshots). Partial repositories and replacement-object
semantics remain unsupported.

### Ahead/Behind Counts

`tests/history.rs` compares `Objects::ahead_behind` with `git rev-list --left-right --count` for
every sampled pair in a 40-commit history with merges, criss-crosses and two roots, with and without
a commit-graph. `girt-cli/tests/git_parity.rs` compares `girt status --branch` with the first line
of `git status -sb` with no upstream, a gone upstream, level, ahead, diverged, a local (`.`)
upstream and a detached HEAD.

Measured with `examples/range_timing.rs` on bare clones of git.git (82,500 commits) and Linux v6.1
(1,138,794 commits), each with a commit-graph from `git commit-graph write --reachable` (generation
v2), Git 2.56.0, macOS arm64, release build, best of 15. girt times are the call alone, after
opening the repository; Git times are whole `git rev-list --left-right --count` processes.
`walk_range` for the matching `A..B` takes the same time as the count:

| Repository | Range                       | Counts     | girt    | Git    |
| ---------- | --------------------------- | ---------- | ------- | ------ |
| git.git    | `master~3...master`         | 0, 6       | 0.01 ms | 10 ms  |
| git.git    | `seen~1^2...master`         | 2, 173     | 0.04 ms | 9 ms   |
| git.git    | `master...seen`             | 0, 327     | 0.06 ms | 9 ms   |
| git.git    | `v2.30.0...master`          | 0, 21,025  | 2.0 ms  | 26 ms  |
| git.git    | `v1.0.0...master`           | 0, 79,570  | 3.2 ms  | 43 ms  |
| Linux      | `v6.1~3...v6.1`             | 0, 13      | 0.01 ms | 7 ms   |
| Linux      | `v6.1~1^2...v6.1~5`         | 1, 150     | 0.09 ms | 8 ms   |
| Linux      | `v6.0...v6.1`               | 0, 15,109  | 8.8 ms  | 49 ms  |
| Linux      | `v5.0...v6.1`               | 0, 326,150 | 42 ms   | 190 ms |

The walk paints both sides in one pass, highest generation first, with colors in an array indexed by
commit-graph position. It stops once no queued commit is reachable from only the side being counted.
To prove an old commit is reachable from only one side, it must still paint the other side down to
that commit's generation. So `v6.0...v6.1`, whose v6.1 side includes branches based on much older
commits, explores 109,164 commits for 15,109 results, more than the default
`HistoryLimits::max_commits` of 100,000. `girt status` passes `HistoryLimits::trusted`, like the
history commands. Opening the commit-graph itself still costs about 20 ms on git.git and 0.7 s on
Linux, which dominates a single near-tip query;
[issue 625](https://github.com/joshka/girt/issues/625) tracks that. Each walk also allocates one
byte of color per indexed commit, about 1 MB on Linux.

Without a commit-graph girt reads both complete ancestries, 0.5-1 s for every row, where Git stops
early using commit dates (60 ms for `master~3...master`). girt does not use date heuristics, so its
answer is exact even with skewed commit dates.

## SHA-1 Pack Writing

`write_pack` exports the caller's explicit object set to separate pack and index `Write` sinks. It
hashes canonical kind/length framing and exact payloads, rejects mismatched identities and
conflicting duplicates, and collapses exact duplicates. `ObjectKind` admits only the four logical
Git kinds. It does not parse payload syntax, traverse references, or require graph closure. This
permits byte-preserving forwarding of objects while leaving graph policy to the caller.

Pack v2 entries and index v2 IDs are ordered by ascending SHA-1 identity. Entries use zlib level 6
without deltas. Identical inputs produce identical artifacts with the same compression backend and
version; dependency upgrades may change compressed bytes. Similar revisions can take substantially
more space than delta-selected packs. The opt-in delta writer is described
[below](#bounded-pack-delta-compression). SHA-256, thin packs, repacking, pruning, GC, multi-pack
indexes, and transport are excluded. No dependencies were added.

`PackWriteLimits` bounds input occurrences before allocation/sorting, each payload, total input
bytes including duplicates, and both output lengths. Payloads are borrowed and compressed directly
into the sink; metadata grows with input count and zlib uses its own scratch space. Limits are not a
process-memory ceiling. Pack counts fit `u32`; byte counters use checked `u64` arithmetic. Index
large-offset references have 31-bit slots. Output checks include trailers and never write beyond the
configured byte limit.

Input validation failures leave both sinks untouched. Later failures may leave partial artifacts, or
a complete pack and incomplete index. Both must be discarded. Success includes flushing both sinks,
not filesystem synchronization. Sinks must start at artifact offset zero. Buffered sinks are
recommended for files because index tables are emitted incrementally.

The API does not install artifacts. It never removes loose objects or existing packs. The example
assembles a new, private repository before any readers exist; its renames are not a live publication
protocol. Validated received-pack installation is now provided separately by
[fetch](#upload-pack-fetch), with index-last discovery and no-clobber publication. The writer itself
continues to produce caller-owned artifacts only.

Original writer code and synthetic fixtures follow the public
[Git pack format specification](https://git-scm.com/docs/pack-format); no Git source or tests were
copied. `tests/packs.rs` exports Git-generated mixed-kind payloads and original binary/4 MiB blobs,
then runs independent `git index-pack --index-version=2`, compares the complete regenerated index,
runs `git verify-pack`, and checks exact `git cat-file` and girt reads from pack-only private
stores. Empty packs and repeated input objects are included. Tests beside the writer cover
deterministic ordering, identity/kind mismatches, duplicate conflicts, short writes, write/flush
errors, limits, header boundaries, and counter overflow.

The original synthetic index fixture checks offsets immediately below and at 2 GiB and at 4 GiB,
including exact 32-bit slots, 64-bit table bytes, checksums, and reader interpretation on 64-bit
hosts. It does not allocate or validate an actual multi-gigabyte pack. Git 2.55.0 on macOS 26.6.2
arm64 was exercised; other platforms and actual multi-gigabyte output remain untested.

## Pack Index Rebuild

`girt::pack::rebuild_index` regenerates a pack's `.idx` from the `.pack` alone, as
`git index-pack <pack>` does, and replaces the old index atomically. It verifies a private copy of
the pack with the received-pack importer: trailer hash, entry framing and CRCs, every object's
identity, and every delta resolved inside the pack. External bases (thin packs) are refused. The
index is version 2 with 64-bit offsets past 2 GiB, which is Git's default, and is parsed back before
publication. A `tmp_idx_*` file in the pack directory is synced, takes the pack's mode, and is
renamed over the index; then the directory is synced. A byte-identical existing index is left
untouched, and an existing index is compared in bounded chunks rather than read whole. The pack copy
is bounded by `max_pack_bytes` and checks cancellation per chunk. If the directory sync fails after
the rename, `Unsynced` reports the published index rather than claiming no change. A pack that fails
verification is refused with the pack directory unchanged.

`tests/pack_index_rebuild.rs` uses `tests/support/pack_git.rs` fixtures (SHA-1 and SHA-256, OFS and
REF deltas). It damages the Git-written index's checksum, fanout, a name, an offset (outside the
pack, duplicated, swapped), the trailer pack hash, or truncates or deletes it. Each rebuild is
compared byte for byte with the index `git index-pack --index-version=2` wrote, then checked with
`git verify-pack` and by reading every object. Damaged packs (body byte, trailer, truncation) are
refused, also by `git index-pack`, and the pack, index and directory listing are unchanged. A reader
thread racing 50 replacements sees only complete indexes. Exercised with Git 2.56.0 on macOS arm64.

Limitations: only version 2 packs are indexed (Git also accepts version 3, reported here as
`PackVersion`); no v1 output (a v1 index is replaced by v2); `.rev` and multi-pack indexes are not
written; running concurrently with maintenance that retires the same pack can leave an `.idx`
without its `.pack`, as with `git index-pack`. That loses no objects and is repaired by deleting the
index. The private copy needs free space equal to the pack in its directory.

## Upload-Pack Fetch

R16 established the original upload-pack acceptance boundary. R27 extends it with SHA-256 wire
transfer, verified external thin-pack bases, and explicit shallow/depth negotiation; see
[R27 evidence](evidence/r27.md) for the current supported boundary. The original test and resource
claims below describe that earlier revision.

Protocol v2 ([#158](https://github.com/joshka/girt/issues/158)) is supported on caller-owned
streams, smart HTTP and Unix SSH. `Repository` workflows request the `protocol.version` value (v2
when unset, as Git does) and accept a v0/v1 answer. A v2 session reads the capability advertisement,
lists references with one `ls-refs` (`peel`, `symrefs`, advertised `unborn`, optional `ref-prefix`),
then sends one `fetch` command with wants, `shallow`/`deepen`, up to 32 verified haves and `done`.
The response may contain `shallow-info` and must then contain `packfile`; `acknowledgments` after
`done`, `wanted-refs`, `packfile-uris` and unknown sections fail. Received packs take the same
validation and installation path as v0. `filter`, `include-tag`, `want-ref`, `packfile-uris`,
`sideband-all`, `server-option` and additional negotiation rounds are not requested. Receive-pack
(push) always speaks v0/v1.

`fetch::receive` implements a single protocol v0 upload-pack session over blocking `Read`/`Write`
streams. `fetch::receive_local` is the local transport adapter: it starts a trusted local
`git upload-pack` server. It does not use `git fetch`, `fetch-pack`, `index-pack`, or Git parsers on
the client path. The optional [HTTP adapter](#smart-http-and-https) is separate; SSH and credential
discovery are not provided.

The client exposes complete byte-preserving advertisements, capability tokens, and peeled tag hints.
The caller selects explicit advertised tip IDs; peeled hints are not wants. Empty selection sends a
flush and expects EOF. Nonempty selection requires `side-band-64k`, requests `ofs-delta` only when
advertised, sends no haves, then sends `done` and requires NAK. This deliberately simple negotiation
requests complete history on every fetch, including incremental fetches. It sacrifices bandwidth to
keep the received graph independent of destination state. Thin/shallow/filter features, include-tag,
are never requested by the full-transfer entry point. The incremental entry point below also
supports bounded multi-ACK. v1/v2 and non-SHA-1 advertisements fail explicitly.

Packet lengths, aggregate wire bytes, advertisements, refs, wants, and pack bytes have explicit
bounds. Channel 1 carries pack data, channel 2 reaches the caller's progress callback, and channel 3
or `ERR` fails with the peer's message bytes. Truncated packets, unexpected negotiation/channel
messages, missing final flush, and bytes after that flush fail. Streams must end at EOF; this is not
a reusable or stateless HTTP session. The local adapter also requires a successful server exit.

Import checks pack v2 framing, object count, SHA-1 trailer, exact zlib entry termination, and delta
programs. OFS_DELTA and forward/backward REF_DELTA resolve only inside the received pack; external
bases fail even if present locally. All resolved objects receive independently computed identities,
and duplicate identities are rejected. girt generates index v2 offsets, CRCs, fanout and checksums
from the received entries. Reachability from each selected tip checks commit trees and parents, tree
child kinds, and annotated tag target kinds using girt's parsers. Gitlinks refer to separate
submodule stores and are not followed. Tree names and ordering are validated. This is supported
syntax/connectivity validation, not full `git fsck`, signature verification, or SHA-1 collision
detection. Unreachable extras get pack/identity validation but not structured-payload validation.

Object count, individual payload/program sizes, cumulative inflation/reconstruction bytes, delta
depth, dependency-queue visits, and reachable edge occurrences are bounded. Received packs stay in
temporary files; a 96 MiB cache retains decoded delta bases. A dependency queue resolves forward
chains without rescanning every entry, and connectivity reads through the indexed pack. Decoding
after cache eviction is charged to the same cumulative budget. Index/table memory grows with object
count; graph metadata grows with objects/edges. Parsing can copy structured payloads. These are
input/work bounds, not an allocator or process-memory quota.

Cancellation is checked between I/O calls, packets, objects, and graph steps, and a progress
callback can cancel. Interrupted I/O is returned without retrying. A flag cannot interrupt a blocked
caller-owned stream read or an object hash/parse; pack inflation checks between chunks. The local
adapter now provides [owned transport interruption](#owned-transport-interruption), including
absolute deadlines and process-group cleanup on macOS/Linux. Generic streams must provide their own
interruption.

Native local fetch can separately report typed source-reading and pack-construction events through
`receive_local` or `FetchRequest::receive_local` with a `LocalFetchObserver`. Source reads count
unique selected objects, using decoded structured payloads and packed blob framing; known
complete-history boundaries are excluded. Their total is unknown during traversal. Packing counts
only selected outgoing entries. Completing those counts does not establish a finished index. The
terminal `Complete` event follows construction and the final cancellation/deadline check, before
installation or reference publication. Observers run synchronously, must return promptly, and can
request cancellation through `TransportControl`. Cancellation from the terminal event cannot undo
completed construction.

### Installation and Reference Policy

The received result owns a validated temporary pack file and index bytes, including native local
transfers. Drop removes temporary files without changing repository files. Explicit `install` writes
and syncs private temporary files in the destination pack directory, then publishes the pack first
and index last without replacing either path. Identical existing files are reused; different bytes
fail. Readers discover indexes, so an unindexed pack is invisible and an index must have its
complete pack. Existing readers retain open pack/index handles; new readers see the old object set
or the new pair. Concurrent deletion or repacking by other tools can still require an opening retry.
Paths must be trusted, and callers must coordinate pruning/GC until references protect the objects.

Failure never removes or overwrites preexisting objects. Failure between publications may leave a
complete unindexed pack; retrying the same received result can finish it. Ordinary failures clean
private temporaries; crashes may leave them. Directory entries are not synced, so power-loss
durability is not promised. Installation does not repair corrupt existing loose objects that shadow
packed objects. Reopen and read with suitable limits before using the destination as a source.

The lower-level object-transfer APIs never update refs, reflogs, or `FETCH_HEAD`. Callers may use
the existing explicit `update_without_reflog` operations after installation, with expected old
values. Each update is independent; failure of a later update does not undo earlier successes. No
multi-ref atomicity is implied. Remote configuration, refspecs, automatic tag following, pruning,
shallow/partial stores, authentication helpers and push remain outside this capability.

### Fetch Evidence and Provenance

Original code and synthetic fixtures follow the public
[pack protocol](https://git-scm.com/docs/pack-protocol),
[capability](https://git-scm.com/docs/protocol-capabilities), and
[pack format](https://git-scm.com/docs/gitformat-pack) specifications. No Git source/tests were
copied. `tests/fetch.rs` starts disposable local upload-pack servers using independently generated
Git objects. It covers all object kinds, actual server-produced deltas, selected branches/tags,
empty, repeated and incremental fetches, exact Git/girt payload agreement, strict Git fsck, and
byte-for-byte agreement with independently regenerated Git indexes. Local tests exercise
malformed/truncated protocols, peer errors, cancellation/interrupted I/O, corrupt packs,
internal/absent bases, missing and mistyped connectivity, limits and exact boundaries. Publication
tests cover conflicting paths, retry after index failure, preserved existing objects, concurrent
publishers/openers, old snapshots, and concurrent reference changes with conditional-update
rejection.

The runnable `cargo run --example fetch_local` builds both repositories from scratch, fetches three
objects, publishes a branch conditionally without a reflog, and reads the fetched blob. The
[Criterion baseline](benchmarks.md#fetch-baseline) measures advertisement processing and replayed
protocol/import/connectivity separately from server execution and disk publication. Git 2.55.0,
rustc 1.98.1, and macOS 26.6.2 arm64 were exercised. Other platforms, actual multi-gigabyte packs,
hostile filesystems and concurrent GC are not covered by this evidence.

## Receive-Pack Push

The current R29/R30 command model supports create, update and exact-old deletion under valid full
`refs/` names. Wire streams, HTTP and Unix SSH support both object formats. `push-options` and
`delete-refs` are capability-gated; the client requests `object-format=sha256` for SHA-256 peers.
Each command retains its caller-supplied old ID, so a stale command can be rejected independently
without turning into an unconditional overwrite. Native local push uses girt storage, not the
historical receive-pack subprocess described below. It supports both formats, conditional mixed
commands, receive restrictions, hidden refs, explicit-identity reflogs and advertised push options.
On macOS/Linux it runs supported receive hooks with controlled cancellation. Wire push negotiates
report-status-v2 and can retain bounded sideband progress. See [R29 evidence](evidence/r29.md) for
commands and [R30 evidence](evidence/r30.md) for outcomes and remaining limits.

R30 distinguishes a complete acknowledged result from a transport or publication failure. A failed
first write is not sent. Each ref report marks whether its command packet may have reached the peer;
later commands beyond a short upload remain unattempted. Missing outcomes for attempted commands are
uncertain. V2 `ok` packets remain evidence even if rewrite options are incomplete.
`RefStatus::rewrite_complete` identifies that case, and `all_succeeded` requires a complete block.
Proc-receive `option` lines retain actual refname, old/new IDs and forced-update flags. Optional
sideband channel-2 progress is returned as untrusted byte frames; channel-3 fatal messages never
become per-ref rejection. HTTP and SSH retain received status and progress even when their owned
exchange ends with cancellation, deadline or process failure.

Native local push uses conditional per-ref transactions. `LocalPushRequest::with_identity` supplies
the identity for reflogs when `core.logAllRefUpdates` or an existing log requires an append; without
it, the push refuses before object installation. Deletion removes the edited name's log. The report
retains separate reference and log effects on publication failure, so an appended or failed log is
not inferred from the ref acknowledgement. Current-branch refuse, ignore and warn modes are
supported, as are `receive.denyDeleteCurrent`, `receive.denyDeletes`, `receive.denyNonFastForwards`
and ordered `transfer.hideRefs`/`receive.hideRefs` restrictions. The native adapter refuses
`updateInstead`, proc-receive, reference-transaction and push-to-checkout hooks, and other
unsupported receive policy before mutation. Girt starts supported hooks directly without a Git
transport subprocess. jj still owns its pre-push policy.

Incoming objects are quarantined during native `pre-receive`, with existing primary objects as an
alternate. Pre-receive rejection/cancellation preserves primary objects and refs. After acceptance,
girt publishes the original validated artifact and removes quarantine before `update`; update
rejection leaves published objects, as observed with Git. Hooks that modify private object storage
are explicitly unsupported and refuse before publication. Cleanup preserves a replaced quarantine
directory and reports its path, cleanup error and any earlier operation failure, including uncertain
published-ref evidence. Pre-receive pushes write the pack twice, into quarantine and primary
storage. The supported single-pack quarantine inventory is bounded to 16 files/directories and
pinned by SHA-1/SHA-256 installation tests. Inherited object-directory and alternate environment
variables are cleared; pre-receive sees primary alternates rather than the caller's environment
alternates.

Hook pipe/exit waits poll cancellation and deadlines; retained combined stdout/stderr across all
hooks is bounded by `PushLimits::max_status_bytes`; excess output is discarded while draining
continues. Successful hooks preserve background work and drain descendant-held pipes to EOF.
Cancellation, deadlines and errors kill the owned process group and reap its leader.
Escaped/daemonized descendants are outside this containment
([issue 58](https://github.com/joshka/girt/issues/58)). Hooks are trusted executable code and can
mutate storage themselves. Caller exclusion of GC, layout/object-directory replacement and checkout
changes remains required; path identity checks do not establish hostile-race protection. Hook
execution is supported on macOS/Linux; other platforms refuse configured hooks before mutation. No
library signal handler is installed. This does not claim crash durability or full hook parity.

The remainder of this section records the original R13 receive-pack implementation and its
validation scope; its SHA-1, namespace and process restrictions do not describe the current API.

`push::PreparedPush::new` prepares an immutable command list and a pack in the source object format.
Empty receiver roots select a full transfer; verified receiver roots permit exclusion and thin delta
bases. `push::send` implements the receive-pack v0 client over caller-owned blocking streams;
`push::send_local` starts a trusted local Git receive-pack server with binary pipes. No Git client
command selects objects, builds the pack, applies client force policy or parses status. The adapter
clears inherited environment except PATH, disables system/global configuration and forces v0. Local
server configuration and hooks still apply. Both the executable and destination must be trusted.

### Commands and Reachability

Commands name full `refs/heads/` or `refs/tags/` destinations and carry an expected old value,
desired nonzero new ID, and explicit force policy. `None` expects absence; `Some(id)` expects that
exact nonzero ID. Duplicate destinations, HEAD, other namespaces and deletion are rejected.
Advertisement mismatch rejects the whole request before commands are sent. Hidden refs cannot
satisfy an expected existing value. Server old-value checks protect against races after discovery;
force does not remove the expectation.

Branch tips must be commits even with force enabled. By default, an existing branch can advance only
when the old ID is reachable through the new commit's parent graph. An existing tag can retain its
ID but replacement requires `ForcePolicy::Allow`. Creation is allowed, and tags may directly name
any supported object type. Explicit force permits branch rewinds or tag replacement; it cannot
bypass server configuration or hooks. Same-ID commands are still sent conditionally and can be
rejected by server policy. Local refs, tracking refs, configuration, reflogs and working-tree files
never change. The server controls its own ref/reflog and object-storage effects.

Selection follows all commit parents and trees, typed tree children and annotated-tag targets,
including nested tags. Gitlinks are external submodule commits and are not followed. Every selected
object must exist locally. Structured objects pass identity and supported payload validation; packed
blobs are located without retaining decoded payloads. Copied identities come from the source index
and remain hints until the receiver independently imports the pack. Source CRCs are checked while
copying. A packed fallback read can decode shared bases again. All typed edges are checked. A
missing or invalid object fails preparation before any remote command. This is not a claim of full
Git fsck equivalence.

Both writer policies reuse stored zlib streams, including deltas whose same-pack base has already
been written within the output depth limit. Other deltas are reconstructed. Push defaults to bounded
delta search for fresh and reconstructed objects, with whole-object compression explicitly
selectable through `PushLimits::compression` or the configured `PushOptions::with_compression`
builder. Earlier output objects can be internal bases; identity-checked blobs and trees marked
reachable from an advertised receiver root can be external bases. Only roots supplying an emitted
external base join the existing live advertisement recheck. External candidate reads count against
the writer's aggregate input budget; insufficient remaining budget falls back to whole objects.
Search tries nearest-size blobs and trees within the bounded candidate, size and work policy, skips
payloads under 64 bytes, and stops after a completed delta below 1% of target size. It does not
reproduce Git's matching heuristics. Transfer deltas use REF_DELTA framing, requiring no optional
OFS capability. A thin pack must be independently imported and repaired against receiver objects
before installation. Packs offering receiver candidates cap output depth at one so every internal
base is an ordinary entry resolved before external lookup; complete packs without receiver
candidates retain their configured depth. Its companion index is generated into a sink, not sent or
installed. Repeated and incremental pushes using `new` with empty receiver roots retransmit full
selected histories; explicit receiver roots can reduce them as described below.
`PreparedPush::new_with_progress` reports unique source reads and successfully encoded pack entries
through `PreparationProgress`. Reads include receiver history later excluded from the pack; packing
totals describe only selected outgoing objects. Its terminal `Complete` event follows pack
construction and the final cancellation check. It does not report upload, server receipt, acceptance
or publication. Observers run synchronously and must return promptly; setting the cancellation flag
from an intermediate event stops preparation at the next cooperative check. Cancellation from the
terminal event cannot undo completed preparation.

Preparation releases selected payloads after spooling the pack; the caller can drop its object
reader before connecting. Inputs, graph objects/bytes/edges, cumulative ancestry visits/parent
edges, individual reads, pack/index output, command bytes, advertisement bytes/entries and status
bytes have explicit bounds. These bounds exclude allocator overhead, the caller's preexisting
snapshot, and server memory. Read decoding budgets apply per object, not across the selection.

### Status, Failure and Transport Boundaries

Nonempty pushes require and request `report-status`. Unknown optional capabilities are ignored.
Protocol v1/v2 advertisements, SHA-256, shallow advertisements and report-status-v2-only servers are
rejected. No atomic, sideband, deletion, push-options, signed-push or report-status-v2 capability is
requested. Receive-pack `.have` entries are validated and counted and can confirm an explicit
receiver root used for exclusion; fetch peeling hints are not accepted as receive-pack refs. The
wire framing implementation is shared with fetch, while service-specific negotiation and parsing
remain separate.

A complete `PushReport` preserves unpack status and one result per command in caller order,
retaining rejection messages as bytes. `Ok(report)` can contain complete rejection or partial
success: callers must inspect `all_succeeded` and individual results. Multiple refs are never
promised atomicity. Missing, duplicate, unrequested, contradictory or malformed status, premature
EOF and trailing bytes are errors. Server success is an acknowledgement, not a durability guarantee
or a claim that another writer cannot subsequently change a ref.

`PushError::NotSent` means no commands were attempted. Once transmission starts, transport,
cancellation, bound or protocol failure becomes `Uncertain`, preserving every valid status received
so far. Missing acknowledgements mean unknown outcomes, not rejection. Inspect destination refs
before retrying; killing receive-pack does not roll back updates already applied. Even a complete
report is retained when final EOF or local process completion fails. A successful unpack alone does
not establish any ref update. Rejected pushes may leave server-side objects, and hooks can have
independent side effects.

Cancellation is checked between I/O calls, packets, graph steps and pack writes. Interrupted
protocol I/O is propagated without retrying. A flag cannot interrupt a blocked stream, a single
storage read, hash, parse or compression call. The local adapter provides
[owned transport interruption](#owned-transport-interruption) for blocked pipe and server-exit
waits, including stalled hooks. Caller-owned streams must provide interruption/deadlines if needed
and must be closed after errors. Streams are one-shot and must end at EOF after the final status
flush.

SSH, credential discovery, remote/refspec configuration, automatic force, pruning, thin packs,
deletion, atomic multi-ref push, report-status-v2/proc-receive rewriting and server infrastructure
remain deferred. Empty command lists exchange only advertisement and flush, with no pack or status
report.

### Push Evidence and Provenance

The implementation and synthetic packets are original, based on the public
[pack protocol](https://git-scm.com/docs/pack-protocol) and
[capability](https://git-scm.com/docs/protocol-capabilities) specifications and observable server
behavior. No Git implementation or test expression was copied; no dependencies were added.
`tests/push.rs` uses disposable repositories and independently Git-generated OFS/REF-delta sources.
It verifies empty destinations, branch creation/advancement, annotated tags, repeated publication,
all object kinds, exact Git/girt payload agreement and strict Git fsck. Original girt-written
fixtures also cover nested tags, tree nesting, binary blobs, symlinks and missing external gitlinks.

Failure tests cover stale advertisements, an update-hook ref race, explicit force with and without
server permission, checked-out-branch rejection, pre-receive rejection, and partial success with an
update hook. Unit tests cover malformed/truncated status, unsupported protocol/capabilities, missing
or mistyped graph edges, malformed reachable payloads, duplicate commands, resource bounds and exact
limits, short writes, interrupted reads/writes, flush failure and cancellation. Unknown results
remain distinguishable from acknowledged rejection, and valid status prefixes survive truncation.

`cargo run --example push_local` creates two private repositories, publishes a branch and annotated
tag, checks the report and reads the transferred blob. The
[Criterion baseline](benchmarks.md#push-baseline) separates reachable selection/read/validation/pack
construction from prepared-protocol replay. Git 2.55.0, rustc 1.98.1, macOS 26.6.2 arm64 and Apple
M2 Max were exercised. Other platforms, multi-gigabyte packs, concurrent source GC, hostile
filesystems and blocked-I/O cancellation are not established by this original push baseline. Later
platform and transport evidence is recorded separately.

## Owned Transport Interruption

`TransportControl` carries a borrowed cancellation flag and an optional absolute monotonic deadline.
`fetch::receive_local` and `push::send_local` use it for owned local Git servers. Explicit transport
controls interrupt blocked pipes when their flag is set. Generic `receive` / `send` streams retain
cooperative cancellation: only their owner can arrange to unblock an arbitrary `Read` or `Write`
implementation.

The deadline expires at the caller's chosen `Instant`, includes time already elapsed before entry,
and is never reset by traffic. Checks run before path resolution/spawn, at each owned pipe
operation, and during server-exit waits. Waiting polls use at most 20 ms, subject to scheduler
delays. Path resolution, spawn, callbacks, decoding, hashing and other synchronous computation
remain outside forced interruption; no whole-call hard real-time bound is claimed. Push preparation
and subsequent fetch installation are separate operations. Cleanup and kernel-delayed child reaping
can extend elapsed time beyond the deadline.

Cancellation takes precedence if both controls are observed together. Already-returned protocol
errors are not overwritten during cleanup. Each pipe attempt checks control before reading/writing;
bytes merely buffered by the OS do not count as known status. At the final server-exit wait, an
already observable exit wins over a racing cancellation/deadline. Otherwise interruption returns
`Cancelled` or `Deadline`, distinct from peer rejections. Push failures after attempted transmission
are uncertain, preserving every valid unpack/ref acknowledgement, including a complete report if EOF
or exit is still pending. Killing a server cannot roll back ref updates or hook side effects.

On macOS/Linux, servers start in their own process group. Nonblocking pipes and polling run on the
calling thread; there are no background I/O workers to detach or join. Stderr is drained with
bounded work and discarded so a full diagnostic sink cannot block the operation. Protocol
progress/status remains available. On completion, error or unwinding, cleanup sends SIGKILL to the
owned group before reaping the direct child. Exit observation uses `waitid(WNOWAIT)` to reserve the
leader's PID until the group is signalled. Callers must not reap these children globally or enable
automatic SIGCHLD reaping. Descendants which deliberately leave the group are outside this
trusted-server contract; girt signals remaining group members but cannot reap grandchildren.
Unrelated process groups are never intentionally signalled. Windows and other OSes explicitly return
unsupported before spawn.

Material compatibility changes are interruptible local flags, discarded rather than inherited
stderr, group cleanup even on success, removal of upload-pack's implicit 30-second idle timeout in
favor of caller-controlled deadlines, new deadline error variants, and explicit unsupported local
adapters outside macOS/Linux. The examples use 30-second absolute deadlines. The target-specific
`rustix = "1"` dependency supplies safe nonblocking/poll/wait/signal operations; its MIT or
Apache-2.0 license options are compatible with this crate (already a transitive dependency through
tempfile).

Original finite shell servers exercise silent advertisement, stalled reads/writes, final EOF and
exit waits, partial/complete push acknowledgements, full stderr/stdout pipes, cancellation races,
pre-spawn interruption, child reaping, unwinding and descendant cleanup without harming an unrelated
child. Scripted servers are independent fixtures, not copied Git implementations. Disposable Git
pre-/post-receive hooks establish cancellation after transmission, unchanged refs before commit, and
already-changed refs after commit. Pre-spawn push interruption leaves refs and objects absent. No
transfer contacts a network or a real remote.

Runtime evidence for this change is macOS arm64 with Git 2.55.0. The parent revision's Linux/Windows
CI evidence above does not cover these new paths; this change has not been published or run
remotely.

Validation passed `just check` (817 unit, integration and documentation tests, formatting,
all-target Clippy and docs.rs), warning-denying private Rustdoc, both local examples and
markdownlint-cli2. Warning-denying library Clippy cross-checks passed for
`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-gnu` and `x86_64-pc-windows-msvc`. These are compile
checks, not target runtime checks.
[Owned transport benchmark evidence](benchmarks.md#owned-transport-baseline) retains local
process/import/cleanup measurements and source fingerprints.

## Bounded Incremental Transfers

The full-transfer entry points remain available as reproducible baselines. Fetch adds
`KnownHistory::new`, `receive_with_known` and `LocalFetchRequest::with_known`. Explicit local roots
are read with verified identities and complete typed connectivity before use. Trees, commit parents
and tag targets are followed; gitlinks remain external. Missing, corrupt or mistyped local history
fails preparation. No local refs are inferred and no shallow or partial history is accepted.

Negotiation follows the public [pack protocol](https://git-scm.com/docs/pack-protocol) and
[capability specification](https://git-scm.com/docs/protocol-capabilities), implemented
independently. The client sends one batch of at most 32 verified commits followed by `done`,
requesting `multi_ack` when advertised. Without that capability it sends at most one have. Each
continuation must name a unique offered ID, followed by a plain ACK of an offered ID; NAK is
accepted when no continuation was received. Unsupported states, truncation and excess replies fail.
The fixed small batch avoids mutually blocked pipes while both peers write negotiation packets. This
is a bounded strategy, not an optimal ancestry search: distant shared history outside the batch can
be retransmitted.

Locally known wants are omitted from wire wants. An entirely known selection sends a flush and
receives no pack, retaining selected IDs and dependency evidence. Received delta bases must still be
internal, including when a matching local object exists. Connectivity checks traverse the union of
received and verified local objects. The result retains IDs only for local objects actually used by
that traversal. Installation reopens the destination with default `PackLimits` and rechecks their
identities before creating pack artifacts, including on no-op fetches. GC/pruning must remain
coordinated through installation and separate ref publication; this API takes no retention lock.

Local verification separately bounds root occurrences, unique objects, retained payload bytes, edge
occurrences and decoding per read. Decoding work is at most the object-count bound times the
per-read bound. Receive/import budgets remain separate, and the connectivity edge limit now covers
the combined graph. Installation rechecks at most the retained local dependency count and payload
budget, with the same per-read bound. Knowledge preparation is outside the transport deadline;
blocking local I/O, parsing and hashing retain the existing cooperative cancellation contract.

Push adds `PreparedPush::new` with explicit receiver roots. Like
`git rev-list --objects <tips> --not <roots>`, it walks commits from the tips and the locally
present roots together in committer-date order, marks the roots' ancestry known, and stops once
every queued commit is known, after a five-commit allowance for clock skew. Trees of known commits
adjacent to sent commits are marked known before the sent trees are walked. Omission never depends
on dates: an object is omitted only when a mark propagated from a root through parsed edges reaches
it. Skew can only cause redundant sending. A rewritten receiver tip outside the new history still
proves the shared ancestry. As with `git rev-list --objects`, a tagged tree or blob is sent when no
commit is sent even if a known commit's tree contains it. Roots missing locally are ignored. Tags
and shared trees/blobs are supported; gitlinks remain external. Known history is trusted rather than
validated and may be absent locally. Reading it gets a separate `max_edges` allowance, and at most
`max_refs` root occurrences are accepted. Fast-forward proofs walk both tips' ancestry in date order
and answer exactly regardless of dates: a path from the new tip to the old tip never passes through
a strict ancestor of the old tip, so the proof prunes those and ends when no unpruned path remains.

Every root that omitted objects rely on must still appear in the live receive-pack advertisement as
a tip or `.have`. Otherwise `KnowledgeChanged` fails before commands; callers can explicitly retry
with full preparation. Command expectations are independently checked before transmission and again
by the server when committing refs. Races after advertisement retain server rejection or uncertain
outcome semantics; complete and partial statuses are unchanged. Server-side concurrent object
pruning still requires coordination. Packs remain non-thin; ordinary entries remain the default and
bounded delta selection is now opt-in. No HTTP/SSH, credentials, pruning, shallow/partial support or
new ref features are introduced.

`workflow::push` reads the receiver's advertisement before preparing, as `git send-pack` does, and
uses every advertised tip and `.have` as a receiver root, whatever the commands' expected values. A
new branch therefore sends only the history the receiver lacks. Over HTTP the advertisement is the
discovery response and the pack goes in the following POST; over SSH the session stays open while
the pack is prepared. The native local transport reads the destination's references after checking
its receive policy, excluding refs hidden by `receive.hideRefs` or `transfer.hideRefs`. It reads
them again before installing; if a root the pack relies on is no longer a reference target, it
prepares once more against the new references and reports a second change as `KnowledgeChanged`
without sending. Over HTTP and SSH, preparation and the root check use the same advertisement.
Alternates' references are not offered as `.have` lines by the native local transport.

Disposable Git tests cover initial, no-op and incremental transfers, shared/divergent and merge
histories, branch/tag selection, gitlinks, unavailable receiver roots, corrupt/missing local
objects, ACK states and bounds, installation failures without ref publication, expected-ref races
and partial push status. The existing push integration helper now exercises exclusion using expected
old tips, including the rejection and race tests. Fixtures are independently generated with Git
plumbing and girt writers. Measurements and platform conditions are recorded with the
[incremental benchmark](benchmarks.md#incremental-transfer-comparison).

## Bounded Pack Delta Compression

`write_pack_with_compression` accepts `PackCompression::Delta(DeltaOptions)`; `write_pack` and
`PushLimits::default()` retain the ordinary streaming path. No dependencies are added. Options bound
the preceding-entry window (16), suitable candidates (4), eligible payload bytes (1 MiB), search
units per object (8 million), and emitted dependency depth (4). Zero bounds disable search; objects
outside the size bound stream ordinarily. The default minimum complete-entry saving is 16 bytes. The
public Rustdoc defines the work units and scratch-memory estimate; these are algorithmic bounds, not
measured RSS or hard latency guarantees.

Sorting and deduplication still precede selection. Pack and index entries remain in ascending
object-ID order. Candidates come from the preceding window, newest first, with the same kind, length
within a factor of two, and depth below the bound. A fixed 4096-slot anchor table searches byte
sequences without interpreting text or structured payloads. Collisions retain the earliest base
position. Bounded sampling can miss useful matches, especially far into large objects or outside the
window; this writer does not promise Git's compression ratio.

Original delta programs encode exact base/result sizes, literals of at most 127 bytes, and copies
with four offset bytes and three size bytes. A copy size of zero means 65,536 bytes; longer copies
split at 16,777,215 bytes. Encoding follows the published
[pack format](https://git-scm.com/docs/pack-format), without consulting or adapting Git
implementation source. Unit vectors and fixtures are original.

Selection compares actual zlib-level-6 output including the entry header and 20-byte REF_DELTA base
identity. It chooses a delta only when it strictly improves the current best and meets minimum
savings against ordinary encoding. Ties preserve the first candidate. Exhausting search work drops
the unfinished candidate and retains the best completed entry. Fixed inputs/options and compressor
versions reproduce identical bytes regardless of input order. Pack sizes never exceed the ordinary
policy for the same object set, but preparation can cost substantially more CPU and scratch memory.

Only preceding internal bases are used, so there are no forward dependencies, cycles, thin packs, or
external-base retention assumptions. REF_DELTA uses an object identity rather than a relative pack
offset; index offsets retain the existing checked absolute-offset and large-offset encoding. No
OFS_DELTA entries are emitted. This avoids requiring the optional `ofs-delta` receive-pack
capability described by Git's
[protocol capabilities](https://git-scm.com/docs/protocol-capabilities). A server advertising only
`report-status` is explicitly exercised. OFS optimization is deferred.

Push selection runs after receiver-history exclusion, keeping even incremental delta bases inside
the outgoing pack. Cancellation is checked between candidates and every 4096 search units; one hash,
storage call, or zlib compression remains non-interruptible. Input identity, output bounds,
no-clobber installation, force/expected-old checks, and uncertain-outcome semantics are unchanged.

Independent `index-pack` produces the exact same index; `verify-pack -v` proves emitted deltas, and
Git `cat-file` and girt check exact payloads/kinds/identities. Tests include shifted 1 MiB binary
objects, generated histories, empty/tiny entries, and a disposable delta push followed by
incremental transfer. Library tests cover every logical kind without assuming valid structured
syntax. Existing malformed-delta reader tests remain applicable. Evidence is macOS arm64 with Git
2.55.0 and Rust 1.98.1; no new Linux or Windows runtime claim is made. HTTP/SSH, credentials, thin
packs, GC, and repacking remain outside this change.
[Measurements](benchmarks.md#bounded-delta-comparison) retain size, timing, search-count, and
source-fingerprint evidence.

## Smart HTTP and HTTPS

The optional `http` feature adds explicit v0 discovery and upload-pack/receive-pack RPCs. Requests
use exact smart service media types and service preludes. A fetch sends the complete want/have/done
batch in one POST; no-op fetches and empty pushes use discovery only. Full/incremental graph
validation, receiver-history exclusion and optional internal delta compression keep their existing
contracts. HTTP fetch returns a bounded download for explicit synchronous validation and
installation; HTTP push consumes a previously prepared pack. See [HTTP contracts](http.md) for
runtime, authentication, TLS, resource and failure semantics.

Original loopback fixtures invoke actual Git `http-backend` as a CGI service in private
repositories. Git generates the source history, branch, annotated tag and delta pack. Tests compare
girt reads, Git `cat-file` bytes, selected IDs and published refs. Initial fetch/push, a one-commit
incremental transfer, known-only fetch and empty push all succeed; recorded request methods verify
when a POST is omitted. HTTP push also exercises explicit supplied authorization and mixed
accepted/rejected refs through an original update hook.

Independent fixture modes cover authentication rejection, redirects, HTTP 503, malformed/duplicate
headers, wrong media types, content encoding, malformed service prelude, protocol v2, body
truncation and decoded/wire limits. Truncated receive-pack responses retain completed
acknowledgements while unknown refs remain uncertain even when the server has updated them. Complete
Git status followed by incomplete HTTP framing also remains uncertain. Deadlines/cancellation
interrupt stalled discovery, TLS handshakes, a large incompressible upload and status bodies;
partial acknowledgements are retained. Request counts establish that failures do not trigger
retries.

TLS fixtures generate an isolated CA and localhost leaf with OpenSSL, exercise real HTTPS push and
fetch, and reject untrusted chains and hostname mismatch. No real remotes or global trust stores are
used or modified. Fixture code is original; behavior was guided by Git's
[HTTP protocol specification](https://git-scm.com/docs/http-protocol), rather than copied Git
implementation or tests.

New runtime evidence is macOS arm64, Rust 1.98.1, Git 2.55.0, Python 3.14.7 and OpenSSL 3.6.4 on
2026-09-23. Earlier CI for other capabilities does not validate this adapter. Linux workflow
coverage is configured but has not been run for this change; Windows HTTP runtime remains untested.
SSH, credential discovery, proxy use, redirects, protocol v2, shallow/partial repositories and
remote/refspec policy remain outside this change. The HTTP fixture is test infrastructure, not a
supported server.

### Request Headers and User Agent

`tests/http_headers.rs` sends the same configuration through `git ls-remote` and girt discovery to
one loopback listener and compares the recorded `X-*`, `Authorization` and `User-Agent` fields. It
covers several `http.extraHeader` values, an empty value clearing earlier ones, a value without a
colon, leading whitespace, `Authorization`, `http.userAgent`, `GIT_HTTP_USER_AGENT` taking
precedence over `http.userAgent`, and an extra `User-Agent` field replacing the agent. Scoped cases
cover a global entry before and after a scoped one, a less specific scope before a more specific
one, and an empty scoped value. The default agents differ by design (`git/<version>` and
`girt/<version>`) and are not compared. Git sends `Host`, `Git-Protocol` and framing fields from
`http.extraHeader` unchanged; girt refuses them before any request. Git runs with isolated
configuration and empty `credential.helper` and `core.askPass`. Evidence: macOS arm64, Git 2.56.0,
2026-10-18.

### TLS Client Certificates

`tests/http_tls.rs` runs `git ls-remote` and girt's `workflow::remote_head` against one loopback
HTTPS server that requires a client certificate, with disposable OpenSSL CAs and leaves, and checks
that both succeed or both fail. Cases: no client certificate, certificate and key, a combined
certificate-and-key file, a certificate without a key, and `GIT_SSL_CERT` and `GIT_SSL_KEY`,
including the environment overriding `http.sslCert` and replacing an empty `http.sslCert`. Git runs
with isolated configuration, empty `credential.helper` and `core.askPass`, and
`GIT_TERMINAL_PROMPT=0`. Git asks a credential helper for a certificate password when a certificate
has no key; girt fails the connection instead. Evidence: macOS arm64, Git 2.56.0 with libcurl 8.7.1
(LibreSSL 3.3.6), 2026-10-18.

The certificate check has no Git counterpart, so `tests/http_tls.rs` tests it against the same
fixture without a Git comparison. A check pins an otherwise untrusted server certificate, rejects a
trusted one, keeps girt's verdict with `UseDefault`, and overrides an unknown issuer with `Accept`.
Further cases check that a client certificate and `GIT_SSL_NO_VERIFY` still apply when a check is
installed. The check sees `localhost`, no intermediates and girt's verdict. A fetch confirms the
check runs once per connection; with session resumption left on, only the first of three did.

## SSH Transport

The optional `ssh` feature adds one OpenSSH process per v0 upload-pack/receive-pack exchange on
macOS/Linux. Literal endpoint components and an explicit trusted executable/config file define the
boundary. Host-key verification and noninteractive policy are enforced; remote paths are POSIX-shell
quoted. Fetch returns a bounded download for synchronous validation, while push borrows a prepared
pack. See [SSH contracts](ssh.md) for supported inputs, runtime, bounds and recovery.

R24 also resolves R21 SSH URL and scp destinations. A configured command is executed only through an
exact application-approved executable/argument mapping; the `ssh` variant is supported. The
application can select an agent socket or askpass executable for an encrypted key. Disposable
loopback fixtures verify configured Git fetch with both authentication routes. The existing process
owner preserves cancellation, exit and uncertain-push behavior. Windows SSH remains unsupported.

Original isolated sshd fixtures with throwaway keys demonstrate actual Git full, incremental and
known-only fetch; initial, subsequent and no-op push; branches, annotated tags, internal deltas and
receiver-history exclusion. Git and girt read the resulting payloads independently, and Git
verify-pack checks fetch delta entries. Real update-hook rejection preserves mixed ref outcomes;
cancellation during a post-receive hook remains uncertain after Git has committed the ref. Host
trust tests accept the fixture key and reject unknown/changed keys; a different client key fails
authentication. Paths containing spaces, quotes and shell metacharacters round-trip literally.

Separate transport fault services exercise malformed envelopes, bounded wire input, stalled SSH
handshake and Git service, blocked upload, full stderr, status/EOF/exit waits, and cancellation.
Complete or partial acknowledgement prefixes remain visible on uncertain outcomes. Unit process
tests check dropped futures, reaping, cancellation before writes, and group cleanup without killing
an unrelated child. Fault services do not substitute for actual Git interoperability.

Implementation and original fixtures use the public
[Git pack protocol](https://git-scm.com/docs/gitprotocol-pack),
[OpenSSH client](https://man.openbsd.org/ssh) and
[OpenSSH configuration](https://man.openbsd.org/ssh_config) contracts and observed behavior. No Git
source or tests were copied or adapted. New runtime evidence is macOS arm64, Git 2.55.0, OpenSSH
10.3p1 and Rust 1.98.1. No Linux runtime result or Windows SSH support is claimed. Broader async
filesystem/object-store architecture remains undecided.

## Transfer Ownership and Operational Review

The review follow-up preserves exact object bytes, typed graph checks, retained pack snapshots,
owned negotiation history, installation/reference separation, and uncertain push reports. Focused
regressions cover caller-selected installation limits and retry, cancellation before pack output,
artifact paths and reachable-object diagnostic context, and bounded local upload/status progress
with retained acknowledgements. HTTP/SSH lifetime and actual Git interoperability tests exercise the
common downloaded-fetch state.

The original early-rejection fixture fills stdout before resuming stdin consumption. It reproduced a
local write-before-read deadline and now completes with its rejection report. This establishes
finite bidirectional backpressure handling; it is not a claim that a particular Git release emits
that exact response. Actual Git push integration remains separate evidence.

The original forward REF_DELTA fixture in `tests/support/forward_delta.rs` contains unique
eight-byte blobs and literal-only delta programs, with each chain ordered deepest-first. Git
`index-pack --strict` accepts it and `cat-file` returns the expected tip. Girt accepts the
65-object, depth-64 chain with 4,225 resolution visits and rejects a budget one visit smaller.
[Measurements](benchmarks.md#forward-delta-ordering) quantify this supported worst-order shape.

Fixture startup owns its process before parsing readiness, uses a ten-second startup deadline, and
includes retained stderr in failures. SSH readiness uses test-only `serde_json` (MIT OR Apache-2.0,
as declared by the resolved package); no runtime dependency is added.

Review follow-up validation on macOS arm64 passed `just check` (685 unit tests, 314 integration
tests, 12 doctests, formatting, all-target/all-feature Clippy and docs.rs), warning-denying private
Rustdoc, the disposable HTTP/SSH/local-fetch examples, owned-history worker compiler probes, and
Markdown lint. Independent core-only, HTTP-only, and SSH-only library checks also passed. Linux GNU
and Windows GNU library cross-checks with SSH passed. Windows all-feature cross-compilation was
blocked in `aws-lc-sys` by the unavailable `x86_64-w64-mingw32-gcc`; native Windows CI remains the
follow-up for that build. No new Linux or Windows runtime result is claimed.

## Repository Discovery and Initialization

`Repository::discover` searches an existing directory and its physical ancestors up to the
filesystem root, including across mount points. `discover_with_ceiling` includes the supplied
ancestor but never searches its parents. Both paths are canonicalized; the ceiling restricts
candidate locations, not where a gitfile or linked-worktree backlink may lead. A `.git`, `HEAD`, or
`objects` entry selects a candidate for the existing opener. Invalid or unsupported metadata stops
discovery instead of falling back to an outer repository. This conservative marker policy can also
stop at unrelated files with those names. Environment overrides, ownership checks, and Git's
discovery configuration are not used.

`Repository::init(format, path, InitKind)` creates ordinary or bare repositories with
format-specific config, files-backend refs, empty object storage, and unborn `refs/heads/main`.
Ordinary destinations may be existing directories with unrelated files; the three discovery markers
refuse initialization. Bare destinations must be absent, including when the existing directory is
empty. Parents must already exist. Reinitialization is refused without altering existing metadata;
recognized unsupported formats and layouts retain opening errors. Templates, hooks, initial commits,
indexes, separate Git-directory creation, linked-worktree creation, branch-name options, and ambient
configuration are outside this increment. The executable example is
`cargo run --example init_repository`.

Directory creation reserves the metadata destination, and files use exclusive creation. Competing
initializers have one winner. The caller must exclude other writers and path replacement. A later
failure leaves newly created directories and partial metadata for inspection; no rollback or
power-loss durability is promised. Initialization never removes files or overwrites existing files.

Independent fixtures in `tests/repositories.rs` exercise Git object access, commit creation,
reference updates and `git fsck --strict` in both generated layouts, plus Git staging and committing
in an ordinary worktree. Git-generated ordinary, bare, relative gitfile, and linked-worktree layouts
are discovered from nested directories. Reftable configuration is rejected without mutation or
outer-repository fallback. R04 added SHA-256 and R08 added worktree-specific configuration. These
tests derive from the [repository layout](https://git-scm.com/docs/gitrepository-layout) and
[initialization](https://git-scm.com/docs/git-init) manuals and independent CLI observations, not
Git implementation code or upstream fixtures. Existing opening compatibility evidence remains
applicable.

Unit tests cover inclusive ceilings, invalid ancestry and starts, nearest candidates, symlink
ancestry, malformed and dangling markers, exclusive file creation, partial population failures,
concurrent initialization, and preservation of unrelated files. No benchmark is added: this
increment performs small metadata setup and ancestor traversal, changes no existing processing hot
path, and makes no performance claim. Metadata reads retain the opener's unbounded allocation
policy.

Local validation on 2026-09-24 used Git 2.55.0, Rust 1.98.1, and macOS arm64. All 62 repository
integration cases and 33 focused repository unit cases passed, as did `just check` (including all
features), the disposable initialization example, private-item Rustdoc with warnings rejected, and
Markdown lint with the repository's 100-column policy. This increment has not been executed on Linux
or Windows; earlier platform results do not establish its runtime behavior there. Mount-point
crossing follows the ancestor algorithm but was not exercised with a separately mounted filesystem.

## Linked Worktree Creation

`Repository::create_orphan_worktree` creates an absent checkout directory, a registration under the
common Git directory, a private unborn HEAD and an empty v2 index. The branch argument is a full
`refs/heads/` name. A stored branch is refused. Multiple worktrees may point at the same unborn
branch, matching Git. The destination's last native component names the registration; numeric
suffixes resolve name collisions within the caller's bound. Both object formats and files/reftable
reference storage use their respective private HEAD representations.

The common repository's `extensions.relativeWorktrees` setting selects relative forward and back
links. Otherwise links are absolute; `commondir` is always relative. Git can read the result, report
its status and create the first commit. The API does not materialize a tree, create jj workspace
metadata, or switch the main checkout.

The destination and its parent are trusted filesystem paths. The parent must exist; line breaks in
paths are refused because Git's link files are line based. Creation uses exclusive file and
directory operations. A later failure retains any completed registration and destination directory
for inspection, identified in the structured error. There is no automatic rollback or
crash-durability guarantee. The caller must exclude concurrent branch/worktree administration and
destination replacement.

`RepositoryMetadata::create_orphan_worktree_with_options` performs the same creation using the
observed repository layout and returns a `RepositoryLocation`. It never reads shallow history;
malformed or directory-valued `shallow` files therefore do not block this operation. Existing
`Repository` creation methods still reopen the result fully and can report a retained registration
when that opening fails. Relevant reference storage and direct configuration remain validated.

The source default index is ignored. An explicit selected index is locked and validated before
creating the worktrees root, registration or destination. Invalid files, directories and absent
split dependencies fail without those effects. An error after lock acquisition explicitly aborts the
guard; a cleanup failure reports both errors and preserves a replacement lock. Index publication
still checks primary and shared snapshots. Later creation failures retain completed files.

## Linked Worktree Administration

`Repository::repair_worktree` accepts a registration and its existing checkout after a move. It
checks the existing `.git` target, then writes `commondir`, the forward gitfile and backlink with
temporary files and renames. It applies the common repository's `relativeWorktrees` setting and
serializes Windows paths without verbatim prefixes. An error reports successfully replaced files;
leftover temporary files require inspection. Git and noncooperating callers must not move the
checkout or change links during repair.

`lock_worktree` and `unlock_worktree` manage Git's `locked` metadata using an exact reason. A
private administration lock excludes cooperating girt calls; Git does not honor it. Pruning one
registration requires an expired `gitdir` timestamp, an absent target and no `locked` file. An
inaccessible target is uncertain and is never evidence of absence. Pruning can leave a partially
removed registration after I/O failure; the caller must inspect before retrying. Moved checkouts
whose old backlink target is absent cannot be discovered without the caller supplying their new
path. The caller owns workspace/materialization policy and must coordinate moves with pruning.

Inventory checks the shared `commondir` relationship directly instead of reopening every linked
repository. This avoids repeated common config and shallow reads while preserving per-entry link
errors. Full private metadata validation remains available by opening the individual worktree.

## Reference Transactions and Reflogs

`References::transaction` accepts stored direct/symbolic edits and resolved direct updates/deletion.
All stored values and preconditions are checked with the selected names locked. Symbolic discovery
is rechecked after acquiring locks; changes fail preparation instead of redirecting the operation.
Duplicate names, intersecting chains, and ancestor/descendant batches (including delete/create
namespace swaps) are rejected. A stored direct replacement with append logging resolves and locks
its old symbolic chain for the previous log ID, using zero when unborn. It compares the precondition
against the original stored value, then edits and logs only the named ref; the old branch and its
log remain unchanged. Creating a symbolic target or deleting a stored symbolic value still requires
preserved logs. A resolved HEAD edit preserves HEAD and logs the visited chain with terminal old/new
IDs. Direct branch updates do not discover or log aliases/HEAD.

Preparation takes the common packed lock, ref locks in byte-name order, then log locks in byte-name
order. It validates existing logs selected for append. A preparation failure preserves reference and
log contents, although empty directories can remain. Publication removes all selected packed records
before loose deletion, preserving unrelated packed bytes and peel records. It then processes
operations in input order, publishing each ref before its logs. Locks remain held through return.
Packed-only refs can already be absent when a later operation fails.

`TransactionError::Publish` retains a result for every input: reference unchanged, packed record
removed, or publication complete; each requested log is unattempted, appended, or failed with a byte
count. This permits a published ref with an absent/incomplete log. Failures stop further work and do
not roll back successful publication. Short appends can leave malformed tails. There is no snapshot
for readers, filesystem-wide atomicity, fsync, automatic retry, or crash recovery.

`Reflog::Preserve` leaves logs untouched, including malformed logs and deletion. `Reflog::Append`
creates missing logs and appends even on same-ID updates. Deletion retains the log and appends a
zero new ID; it does not adopt Git's CLI log-removal policy. Identity, nonnegative Unix seconds,
offset, and exact single-line message bytes come from the caller. Config, environment, hooks,
fast-forward policy, and object existence/type checks are not consulted. CR, LF and NUL messages and
tab-containing identities are rejected. Ref and log paths share ordinary, bare, separate-gitdir, and
current linked-worktree routing.

Logs are appended, never replaced wholesale. Git's automatic HEAD appends need not honor a HEAD or
log lock, so ordering with those independent appends is unspecified. Callers must exclude reflog
rewrites, expiry and deletion during transactions. The reader accepts Git's omitted separator for an
empty message, preserves non-UTF-8 identity/message bytes, and returns oldest-first records. Numeric
formatting and negative-zero timezone spelling are normalized; arbitrary malformed or legacy
identity forms are rejected. Reads allocate for the complete file and are live, including possible
incomplete concurrent tails. There is no configurable log-size bound or streaming API.

The [workflow example](../examples/reference_transaction.rs) publishes a branch through HEAD and a
tag, handles preparation versus publication errors, and deletes the tag with retained history. The
[benchmark baseline](benchmarks.md#reference-transactions-and-reflogs) measures batch publication
and existing-log validation separately.

Independent fixtures use Git CLI observable behavior and the
[update-ref documentation](https://git-scm.com/docs/git-update-ref) and
[repository layout documentation](https://git-scm.com/docs/gitrepository-layout). No Git source,
upstream tests or copyright-audit material was used. Git reads exact generated records, reflog
selectors, identities, timestamps and ordering; girt reads subsequent Git appends, including empty
messages. Tests cover packed-only/shadowed deletion, linked-worktree private namespaces, and
separate metadata directories. A conditional Git writer races a two-ref batch. Controlled
post-preparation filesystem failures exercise packed replacement, loose publication/unlink, and
first/later log failures; an injected short writer verifies retained append byte counts. Runtime
evidence for this increment is macOS arm64 with Git 2.55.0; no new platform support is claimed.
Expiry/GC, reflog removal, remote/refspec policy, reftable and recovery journals are deferred.

## Remote Configuration and Refspec Mapping

`remote::Remote::find(repository.config(), name)` reads an owned snapshot of four remote keys:
`url`, `pushurl`, `fetch`, and `push`. Names compare exact bytes; key names ignore ASCII case.
Repeated sections and keys retain file order. Empty URLs clear the preceding list for that key;
fetch uses the first remaining URL, and push uses all remaining pushURLs or falls back to all URLs.
Duplicates remain visible. URLs are opaque bytes, not validated transport endpoints or rewritten
addresses. Missing keys give empty lists; a missing named subsection with entries returns `None`.
Empty section headers are not retained by `Config`. Implicit boolean values fail with key and
occurrence diagnostics. Both refspec lists are parsed eagerly, and empty refspec values fail.

`remote::RemoteUrls::find(config, name)` reads only `url` and `pushurl`, with the same
byte-preserving order, reset and fallback rules. It can inspect a remote whose unrelated refspecs
are invalid. `fetch_display_url(config)` and `push_display_url(config)` return the first effective
URL bytes after Git-style `insteadOf` and `pushInsteadOf` rewriting. An explicit `pushurl` receives
only `insteadOf`; otherwise a matching `pushInsteadOf` takes priority for push. These display values
do not apply protocol policy, classify or validate endpoint syntax, or authorize transport.
Malformed rewrite settings still return a configuration error.

`remote::Refspecs` supports explicit full `refs/` names, exact source `HEAD`, one-star mappings
(including partial components, empty captures, slash-containing captures and non-UTF-8 bytes),
leading force intent, negative fetch exclusions, source-only fetch selection, same-name push and
explicit push deletion. Fetch exclusions apply regardless of position. Missing explicit positive
sources fail, including excluded ones; unmatched patterns and exclusions do not. Empty lists produce
empty plans with no implicit HEAD or branch selection.

Plans follow positive-spec order and then source-input order. Exact duplicate mappings collapse at
first occurrence. Distinct sources targeting the same destination fail even if IDs agree; differing
force intent for one destination also fails. This deliberately conservative conflict rule does not
attempt Git's CLI conflict resolution in every duplicate case. Source-only selections may coexist
with destination mappings. Duplicate input names and zero IDs fail even when unselected. Wildcard
substitutions are revalidated as full names before returning a plan. Errors return no partial plan.

Advertisement mapping discards peeled hints and preserves each advertised tip name/ID, including
symbolic HEAD. It neither follows capability aliases nor creates a symbolic local ref. Callers
resolve local symbolic refs before supplying push inputs. A `+` flag records syntax only; ancestry,
expected old values, object kinds, namespace restrictions, destination prefix conflicts and force
authorization remain separate. Mapping can describe push deletion even though current `push`
transports cannot send it. Push preparation may reject namespaces accepted by this general mapper.

The snapshot and mapping APIs perform no network, object lookup, config editing or reference
mutation. Separate `config::Document`, `config::ConfigEdit` and `remote::RemoteConfig` APIs provide
[lossless direct-file editing](configuration.md#lossless-file-editing), with explicit file scope,
held locks, byte preconditions and recovery. Resolved configuration supplies inherited and included
values. URL rewriting, credential discovery, `push.default`, branch selection, matching push,
shorthand names, arbitrary revision expressions, raw object-ID sources, empty/default fetch forms
and `tag <name>` shorthand are outside the supported slice. Other configuration options are
uninterpreted, including mirror, pruning, tag following, partial clone, custom service commands and
transport options. Their presence does not change the four-key interpretation. Consumers must choose
their own orchestration policy rather than treat the result as all of Git's remote behavior.

`examples/remote_plan.rs` reads disposable repository config, selects IDs from a supplied
advertisement, retains destination mappings, and prepares a conditional creation push with an
explicit no-force policy. It sends nothing. Existing fetch selection callbacks cannot directly
return mapping errors: a caller can save the mapping result, return an empty selection on failure,
and inspect that result before accepting the transfer outcome. Reference publication follows
validated object installation as a separate operation with explicit expectations and reflog policy.

Independent fixtures in `tests/remotes.rs` create bare SHA-1 repositories using Git `init`,
`mktree`, `commit-tree`, `update-ref`, `tag` and `config`, then compare actual `fetch`, `push`,
`ls-remote`, `remote get-url`, `for-each-ref` and `rev-parse` behavior. They exercise wildcard and
explicit mappings, exclusion position, empty/partial captures, same-name push, HEAD, annotated tag
identity, duplicate mappings, missing sources, collisions, deletion and URL reset/fallback. Fixtures
use throwaway local paths and isolated Git config; no upstream source, test data or copyright-audit
material informed implementation. Format references are the public
[fetch refspec documentation](https://git-scm.com/docs/git-fetch),
[push refspec documentation](https://git-scm.com/docs/git-push), and
[remote configuration documentation](https://git-scm.com/docs/git-config#Documentation/git-config.txt-remotenameurl).

The [Criterion baseline](benchmarks.md#refspec-mapping) measures expansion over 10 and 10,000 source
refs. Input sizes are caller-bounded; the mapper has no cancellation or hard memory limit. Runtime
interoperability evidence for this increment is macOS arm64 with Git 2.55.0; Linux and Windows
runtime checks have not been performed for this increment.

## Fetch Orchestration

`fetch::FetchRequest` composes an explicit fetch refspec list (including one read from a named
`remote::Remote`) with local, HTTP or SSH transfer and a shared install/publication path. The
endpoint, credentials, history, scheduling, limits, force authorization and reflog policy remain
caller inputs. `examples/fetch_remote.rs` runs against disposable repositories; network tests move
the owned download to a caller-owned blocking worker before validation and publication.

Preparation captures stored destination values before network work. Mapping uses the advertisement
from the actual transfer session/discovery; a preview is not reusable authority for a later
advertisement. Missing exact sources are reported while other valid selections continue; mapping
collisions and destination-policy errors request no objects. HTTP discovery and RPC can observe a
changing server: the request retains discovered IDs and either receives their valid objects or
fails, without silently remapping to new IDs. Git 2.55.0 aborts a CLI fetch containing a missing
exact source before publishing the otherwise valid mapping; girt leaves that decision with the
caller through `FetchReport::missing`. Publication uses conditional transactions for changed refs,
including expected absence; no-op destinations are not locked or rewritten. Optional pruning deletes
only direct `refs/remotes/*` destinations owned by a positive fetch mapping when its source is
absent; negative selectors protect matching destinations. Every delete uses the captured exact old
value. Symbolic remote aliases, tags and other remotes remain outside that prune scope.

`with_depth` sends a positive depth over HTTP, SSH or a caller-owned v0/v1 upload-pack stream. The
workflow locks and compares the shared shallow file against its prepared snapshot, installs the
verified pack, replaces shallow boundaries, then reopens the repository for selected-tip checks
before changing refs. Git may retain a root commit as shallow at a finite depth. The lower-level
`ReceivedFetch::install` continues to refuse shallow input. Callers must exclude other depth writers
for the complete operation; ordinary ref writers may race under exact expectations. Known history
must carry every captured destination boundary. Empty selections cannot change shallow metadata.

Supported destination rules are deliberately narrower than full CLI fetch:

| Destination      | Supported rule                                                       |
| ---------------- | -------------------------------------------------------------------- |
| `refs/tags/*`    | Create any kind; replace only with `+` and exact-name authorization. |
| `refs/remotes/*` | Any kind may be created; replacements use the object rules below.    |
| `refs/heads/*`   | Rejected, including bare repositories and forced refspecs.           |
| Other namespaces | Rejected; symbolic destinations are also rejected.                   |

For remote-tracking replacements, both tips are peeled under explicit tag/read limits. If both peel
to commits, the old commit must be an ancestor of the new one, unless both `+` and explicit name
authorization permit a non-fast-forward replacement. If either tip is a tree or blob after peeling,
replacement needs no force. Missing or unreadable old objects fail closed. Ancestry uses bounded
synchronous history queries; cancellation is observed before/after each query, not within it. Update
limits apply per destination, so aggregate work can multiply by mapping count.

The [git-fetch manual](https://git-scm.com/docs/git-fetch) describes namespace/type-dependent
updates and force requirements for tags. Its statement about arbitrary replacements outside
heads/tags is broader than the Git 2.55.0 behavior observed here: Git rejects remote-tracking commit
rewinds and annotated tags peeled to older commits without force. This workflow follows those
observed stricter rules. Independent CLI tests retain both rejected unforced and accepted forced
cases, alongside accepted tree/blob replacements. No Git implementation or upstream test source was
used.

Branch destinations are rejected to avoid implementing a partial branch-in-use check. Main and
registered linked-worktree HEAD chains must be detached or stay entirely in `refs/heads/*`; chains
into otherwise supported destinations fail closed. Inaccessible/stale registrations are errors. The
checks run at preparation and after installation. Callers must exclude checkout, symbolic
HEAD/branch edits and worktree-registration changes throughout the operation; these checks do not
lock worktree metadata. Ordinary destination writers remain supported through exact expectations.

Validation precedes installation, and installation precedes every ref edit. After installation,
selected-tip graphs are reread through the destination object store under explicit verification
budgets. This catches corrupt loose objects hiding valid installed pack objects, including corrupt
descendants, before publication. `FetchReport` separates transfer statistics, missing exact sources,
object and shallow installation, and successful transaction outcomes. `FetchFinishError` retains the
report and the original transaction error with partial ref/log effects. Installed objects and
published shallow metadata remain after update rejection, cancellation after installation or
publication failure. Installation itself can leave an unindexed pack on failure. No whole-fetch
rollback, atomic visibility or power-loss durability is promised. Callers must coordinate pruning/GC
until publication completes. Reflog identities/messages are explicit and unchanged/source-only
entries do not append logs.

`FETCH_HEAD`, clone, checkout, implicit tag following, credential discovery, global config policy,
config editing and push orchestration are deferred. Existing protocol/storage limitations still
apply. Preparation and reference metadata operations retain their existing unbounded metadata
allocation contracts; this layer introduces no runtime or backend framework.

Original fixtures in `tests/fetch_workflow.rs` generate blobs, trees, commits and annotated tags
using Git CLI plumbing, then compare Git and girt fetch results. Tests cover initial/incremental
fetch, multiple refs, wildcard exclusions, source-only/no-op results, tag force authorization,
commit rewind rules, tree/blob kind replacement, destination races, cancellation, failed
installation (including an unindexed residual pack), missing local dependencies, publication lock
failure, explicit reflogs and main/linked-worktree HEAD aliases. `tests/http.rs` and `tests/ssh.rs`
cover end-to-end publication on owned workers using the existing independent loopback fixtures; SSH
also rejects malformed advertisements without installation. Existing transaction tests retain
partial-publication evidence.

This increment was exercised on 2026-09-24 on macOS arm64 with Git 2.55.0 and rustc 1.98.1; its
[benchmark baseline](benchmarks.md#fetch-orchestration-baseline) records retained measurements.
Earlier Linux/Windows validation records do not establish runtime support for these new workflow
paths.

## Clone Without Checkout

`clone::CloneRequest::prepare_tracking` explicitly selects the remote-tracking reference layout and
composes exclusive destination initialization with existing local/HTTP/SSH fetch, validation,
installation and conditional references. The destination must be absent, including empty directories
and dangling symlinks, and its parent must exist. Downloads and validation precede all destination
writes. No hardlinks, alternates, index, checkout, shallow/partial clone, recursive submodules,
mirror behavior or credential discovery are included. Ordinary clones contain only `.git`; Git can
report staged deletions until a later checkout populates the index and files.

Both bare and ordinary layouts store every advertised branch under `refs/remotes/origin/*` and all
tags under `refs/tags/*`. A selected branch additionally gets one `refs/heads/*` ref and symbolic
HEAD. Bare clones deliberately use this same layout rather than Git CLI bare clone's
all-local-branch layout, so later `FetchRequest` calls can use the persisted refspecs without
weakening its branch and worktree safeguards. No `origin/HEAD` alias, pruning or `FETCH_HEAD` is
written. All configured refspecs are unforced; later replacement policy stays explicit at the fetch
boundary.

`prepare_tracking` names the reference-layout choice; `InitKind` selects storage placement only.
Consumers enumerating or serving `refs/heads/*` see only the selected local branch, or none for
detached/unborn selection. Conventional copies with every branch in `refs/heads/*` are unsupported.

Default selection honors one consistent `symref=HEAD:refs/heads/...` capability. Without a symbolic
hint, an advertised HEAD remains detached even when one or several branches share its ID. A missing
HEAD in a nonempty advertisement requires an explicit full branch name. Explicit selection overrides
remote default-branch hints but does not filter the other branches/tags. Empty v0 advertisements can
omit the remote's unborn branch name; use the caller-selected branch or `main` in that case. An
advertised unborn symbolic name is preserved. Tag/revision selection is unsupported; branch and
detached HEAD tips must be commits, without tag peeling. All advertised branch tips are checked.

Planning occurs inside the actual transfer callback. A preview cannot replace that advertisement,
and IDs never change after download. HTTP discovery and RPC can race remote changes; a server may
refuse an unavailable advertised ID, which is a transfer error with no destination effects. Clone
never substitutes a newer tip or promises that its snapshot remains current at the remote.

Finish reserves the root, initializes metadata, uses fetch to install and verify objects and publish
tracking refs/tags, verifies branch object kinds, saves config under an exclusive `config.lock`,
then conditionally publishes the local branch and HEAD in that order. Config replacement compares
the exact initializer contents and refuses other writers' edits. A final reopen supplies a fresh
config snapshot before success is returned. Direct refs use the caller's reflog policy; stored
symbolic HEAD uses preservation because symbolic targets cannot append reflogs through this
primitive. Detached HEAD honors append policy: its old unborn branch is locked while recording zero
as the prior ID, then HEAD is made direct and its log is appended. A failed append retains the
published HEAD and an explicit partial-log outcome.

Errors retain reservation/initialization flags, a completed fetch report or its detailed nested
failure, config completion and final reference outcomes. Initialization can leave partial metadata;
installation can leave an unindexed pack; later errors can leave installed objects, tracking refs,
tags, config, local refs or partial reflogs. No automatic cleanup, rollback, retry, atomic reader
snapshot or crash durability is promised. Callers exclude path replacement, other writers,
checkout/worktree changes and GC during finish. Cancellation is checked between phases; it does not
interrupt config replacement or a started reference transaction. Join synchronous workers even after
requesting cancellation.

The fixed `origin` stores a caller-supplied credential-free URL and two fetch refspecs, plus the
selected branch's remote/merge keys. The URL is independent metadata, not an endpoint resolver;
transport endpoints, credentials, trust, budgets and deadlines stay explicit. Relative local URLs
are relative to the new repository when consumed by Git. HTTP/SSH return owned downloads and perform
no object validation or storage inside network polling. Validation/finish stay synchronous and
caller-scheduled. Configuration replacement is clone-specific, not a general config editor.

Original fixtures in `tests/clone.rs` use Git CLI `init`, `hash-object`, `mktree`, `commit-tree`,
`mktag`, `update-ref` and `symbolic-ref`. Git verifies objects, strict fsck, no-checkout status and
stored remote configuration; both girt and Git perform subsequent fetches. The fixtures cover bare
and ordinary clones, empty/default/selected/detached or missing HEAD, multiple equal branch tips,
annotated tags, remote changes after transfer, invalid branch objects and unsafe destinations.
Focused unit tests exercise planning, config encoding/locking, dropped transfers, cancellation,
reservation races, installation residuals, verification limits and reference/config failure phases.
HTTP and SSH loopback fixtures exercise owned worker handoff and cancellation before initialization.
Existing transaction tests provide the shared per-ref/per-log partial-publication evidence.

Runtime evidence for this slice is macOS arm64 with Git 2.55.0, Rust 1.98.1 and OpenSSH 10.3p1.
Linux/Windows runtime behavior is not established by this run. Finish requires the Unix reference
backend; local process and SSH transport contracts retain their macOS/Linux restrictions; clone
filesystem interoperability tests are Unix-gated. No dependencies or platform support were added,
and no upstream implementation/test source or copyright-audit comparisons were used as
implementation input.

## Tree Comparison

`Objects::compare_trees` compares explicit SHA-1 tree IDs recursively, with `None` representing an
empty side. It returns owned leaf records with byte paths and optional old/new modes and IDs.
Additions, deletions, content identity changes, executable-bit changes and type changes are distinct
through those values. Files, symlinks and gitlinks are leaves; file/directory replacements become a
leaf deletion/addition plus changes below the directory. Empty directories produce no records.
Results sort lexicographically by raw full-path bytes, with shorter prefixes first. Trees read from
storage must satisfy Git ordering, even though matching uses literal names to align replacements
whose positions differ under Git's file/tree ordering rule.

Equal tree IDs, including equal roots, skip all object reads. Success does not establish existence,
syntax or validity of skipped trees or their descendants. Other traversed trees require verified
storage identity, supported tree syntax and valid names, uniqueness and order. Leaf targets are
never read or type-checked; gitlinks can refer to absent commits in another repository. Missing,
wrong-kind, malformed and corrupt traversed trees produce errors with their identity and byte path.
This is structural comparison, not object connectivity validation or fsck.

Traversal is iterative and read-only. Caller limits bound tree-read occurrences, cumulative tree
payload bytes and entries, directory depth, generated path bytes and output records. Per-read
storage decoding limits also apply. Parsing one bounded payload can allocate its entries before the
cumulative entry-count check; limits bound inputs rather than exact heap usage. Cancellation is
checked between reads and entry operations and around final sorting. A single read, parse,
validation or sort remains synchronous and noninterruptible. Failure returns no partial result and
changes no files. No object cache, store trait, async runtime or general diff framework is added.
Content diff is a separate [operation](#content-diff). Rename/copy detection, pathspecs,
index/worktree comparison, merge and checkout remain outside scope.

`tests/tree_compare.rs` generates original fixtures with isolated Git `hash-object` and
`mktree -z --missing` invocations. It compares IDs, modes and paths with
`diff-tree --raw -r -z --no-renames --no-commit-id --no-abbrev --no-ext-diff --ignore-submodules=none`,
normalizing only record order to the API's documented full-path ordering. Cases exercise both
directions and empty sides in loose and packed storage, including nested changes, prefix ordering,
file/directory replacements, executable status, symlinks, foreign gitlinks and
non-UTF-8/control-byte names. Git `pack-objects --revs` and `prune-packed` create the packed
variant. No upstream implementation or test source is used. Fixtures never create those byte names
on the host filesystem.

The local evidence uses Git 2.55.0 on macOS arm64; native Linux and Windows refresh is separate
work. The comparison adds no platform-specific filesystem or process behavior; it inherits the
existing reader's storage boundary. No additional platform support is established by this slice.

## Line Attribution

`Objects::blame` and `Repo::blame` return consecutive line ranges with commit id, original byte
path, original start line, destination start line and count. Coordinates are one-based; empty files
have no ranges. Both object formats are supported. File renames use the tree rewrite inference
policy. At merges, an identical parent file takes precedence; otherwise unchanged lines pass to the
first matching parent in stored parent order. New lines belong to the merge itself.

`blame_with` takes whole-walk limits and cancellation. Verified blob content travels with pending
history states and is shared across identical-parent edges. Pending states prefer newer committer
dates; groups for the same queued commit and path combine their work. Timestamp-skewed ancestry can
still repeat work, under the same whole-walk bounds. Missing or malformed dates sort oldest and do
not change attribution. History traversal still visits unaffected commits. It returns no partial
result on failure. Incomplete rename inference is a resource refusal, never an assumed file
addition; raise the explicit rewrite bounds to retry. Missing objects are errors, not roots. Reached
shallow boundaries and a NUL-containing or nonregular requested file are refused. Older
NUL-containing versions are compared as raw lines; historical type transitions start a new file
history. Empty files need no history attribution. Copy/move detection within or across files,
ignore-revs, textconv and incremental output are unsupported; `Repo::blame` refuses a textconv
driver. Repeated lines can have different shortest-edit alignment from Git. The shared content diff
applies boundary compaction and indent-aware placement, with the limitations described below.

Original disposable histories in `tests/blame.rs` compare commit, original line and original path
against Git 2.56.0 `blame --line-porcelain` on macOS arm64. They cover edited renames, merges with
edits in both parents, a rename crossing a merge, identical-parent precedence, both hashes and
unterminated lines. Separate fixtures cover byte paths, CRLF/non-UTF-8 content, empty files, missing
history, cancellation and resource refusals. No upstream source or fixtures were used. See the
[real-repository timing evidence](benchmarks.md#line-attribution-baseline).

## Content Diff

`content_diff::diff` accepts borrowed byte payloads and returns unchanged, binary-changed or owned
edit ranges borrowing both text inputs. LF terminates a line and remains part of its bytes. Empty
input has no lines; a nonempty unterminated suffix is one line. CRLF, bare CR, NUL in forced text,
invalid UTF-8 and a missing final LF are significant, with no decoding or normalization. Edit ranges
are zero-based and half-open in both original coordinate spaces. Adjacent edit edges are coalesced;
unchanged gaps are implicit and byte-identical. Results contain zero context and are not unified
patch hunks. Boundary compaction moves equivalent changes without adding context; render the result
with `patch::write_hunks`. The content-diff example escapes bytes solely for display.

Auto mode classifies changed payloads as binary if either contains NUL anywhere. This is an explicit
whole-input policy, not Git's sampling policy. Forced text and forced binary override it; equal
bytes always return unchanged. Binary results retain both slices without encoding a binary patch. No
path, attributes, textconv, external driver, whitespace-ignore, rename/copy, patch-application,
merge, index or worktree policy is applied; patches apply attributes per path (see
[Diff Attributes](#diff-attributes)).

`BlobContent::read` separately loads a tree change's regular, executable and symlink sides from
verified loose/packed storage. Absent sides become empty buffers; the original tree change retains
existence and modes. Symlink payloads are target bytes and are never dereferenced. Trees and
gitlinks are rejected before reads. Equal IDs still require reads, so the adapter establishes
existence and kind rather than trusting structural identity. Missing, wrong-kind and storage
failures retain IDs; the caller keeps path/side context. Per-read decoding limits and a combined
retained-payload limit apply before decoding. No storage or reference changes occur.

### Algorithm and Resource Contract

The original implementation uses the edit-graph frontier algorithm described in Eugene W. Myers,
[An O(ND) Difference Algorithm and Its Variations](https://doi.org/10.1007/BF01840446), section 3
([paper](https://neil.fraser.name/writing/diff/myers.pdf)). Only the published algorithm description
informed implementation. No Git/xdiff implementation, upstream tests or copyright-audit material was
consulted. No dependency was added. A compact stored-frontier implementation keeps the search and
traceback small enough to review, while explicit limits reject expensive cases. Linear-space
refinement, line interning and alternate algorithms are deferred until consumer evidence justifies
the additional implementation.

For N total lines and edit distance D, search takes O(ND) line comparisons and O(D²) trace
positions, plus linear line-offset and output storage. Bytes compared also consume work budget. The
algorithm minimizes inserted plus deleted lines, greedily extends equal runs, visits insertion-heavy
diagonals first and selects deletion when predecessor old positions tie. This establishes
deterministic search alignment. A separate boundary pass rotates byte-identical lines to coalesce
changed groups and aligns them with changes on the opposite side where possible. Without indent
scoring it selects the last equivalent boundary reached by these rotations. This reproduces the
compaction observed through Git's `--no-indent-heuristic` on original fixtures; it does not
reproduce all of Git's choices of an edit path.

The default additionally scores unaligned groups using indentation and blank separators. It scans
forward over a window of at most 100 rotations, bounded further by the changed-group length plus
one. Equal comparisons favour the later candidate. The comparison combines the order of the two
boundary indent sums with penalties for equal/decreasing indentation and rewards for blank
separators; it is non-transitive, so scan order matters. Tabs advance to eight-column stops. Context
inspection stops after 20 blank lines; a run reaching that cap acts like an absent neighbour
(start/end of file) for scoring. The compact scoring rule is calibrated against Git CLI experiments
rather than copied from an implementation. Some synthetic blank-run and file-edge ties still differ.
This is an indent-aware heuristic, not a universal Git scoring-equivalence guarantee.
`content_diff::diff_with_options` and `patch::Options::with_indent_heuristic(false)` disable scoring
while retaining compaction. Blame uses the default shared mapping. Every rotation preserves the
unchanged subsequence and shortest-edit count; no minimum replacement-count, prettiest-patch, or
exact Git CLI-output guarantee is made.

Defaults bound combined inputs to 64 MiB, combined lines to 1,000,000, retained frontier positions
to 1,000,000 and work to 64 Mi units. Each frontier row reserves its full distance-plus-one width
before allocation. Scanning and comparison charge bytes conservatively; frontier, line-comparison
and traceback steps also charge units. Compaction uses linear line markers, charges its scans and
rotations to the work budget, and checks cancellation at each step. Work can exhaust before other
bounds, even for inputs below the byte cap. Empty sides bypass search; equal and binary results
bypass line/trace allocation. Input bounds apply to all modes. Bounds are not exact process-memory
limits; allocator overhead, spare vector capacity, caller-owned payloads and storage snapshots are
separate. Raising limits can admit quadratic work/trace growth. Exhaustion returns an error with no
approximate or partial edits.

Cancellation is cooperative: checks occur before work, within byte scans at 4 KiB chunks, per
frontier/line comparison, during traceback and boundary selection, and before return. Allocations,
deallocation and single storage reads cannot be interrupted. No hard wall-clock bound or async
runtime is introduced.

### Content Diff Evidence

`src/content_diff/tests.rs` contains original named examples, an exhaustive short-sequence corpus
checked against an independent dynamic-programming edit-distance oracle, varied byte inputs,
resource/cancellation boundaries and large shapes. Adapter units test storage failure context and
supported modes. `tests/content_diff.rs` writes original bytes into isolated temporary files and
invokes Git `diff --no-index --text --minimal --diff-algorithm=myers --no-indent-heuristic` with
zero context, no external drivers or textconv, and isolated configuration. Original repetitive
fixtures check exact coordinates for merged insertion/deletion groups and alignment with opposite
changes. Both indent modes also compare original blank separators, file-edge blocks, tabs and
whitespace-only lines. `tests/fixtures/content-diff/observe.py` records a small reproducible CLI
experiment without upstream inputs. The parser preserves patch body bytes and handles Git's
missing-final-newline marker. Simple ranges/bytes agree exactly; ambiguous alignments are checked
through reconstruction, unchanged gaps and equal minimum costs. This does not assert universal
alignment equivalence. A separate simple NUL fixture compares ordinary Git binary classification;
the whole-input sampling difference remains intentional.

Git `hash-object --no-filters` supplies blobs for tree-to-content integration, including CRLF and
non-UTF-8 data. Git repack/prune-packed supplies the packed variant and a removed loose object
confirms packed reads. Tests use only portable files and Git-managed fixture refs, without claiming
girt's reference backend works on Windows. The suite is included explicitly in the Windows job.
Local execution uses macOS arm64, Git 2.55.0 and Rust 1.98.1. New native Linux/Windows runtime
results are uncollected; platform support has not expanded.

## Diff Attributes

`attributes::Attributes` compiles `.gitattributes`-format files supplied with a `Source`; `get`
returns one attribute's set, unset, value or unspecified state for a file path.
`Repository::attributes` reads the files Git reads: the system file (unless `GIT_ATTR_NOSYSTEM`),
`core.attributesFile` or its XDG default, per-directory `.gitattributes` and `info/attributes`.
Per-directory files come from the worktree, from a tree named by `GIT_ATTR_SOURCE` or `attr.tree`,
or from nowhere in a bare repository (`Repository::attribute_source`). Git reads neither the index
nor the compared trees when comparing two trees, so neither does girt. Worktree files are read on
demand without following symlinks; a symlinked `.gitattributes` is skipped, as Git skips it.

Lines follow Git: later lines and later assignments win, a pattern matches as in `.gitignore`
relative to its directory, a pattern ending in `/` matches no file, negative patterns and lines with
an invalid attribute name are ignored, and quoted patterns use C-style escapes (an unquotable quote
stays literal). Macros are honoured in every source except a `.gitattributes` below the root;
`binary` expands to `-diff -merge -text`, and an unset or unspecified macro does not expand.
`core.ignoreCase` folds ASCII case in patterns and directory prefixes.

`patch::DiffAttributes` applies the `diff` attribute per side: set forces text, unset (including
`binary`) forces `Binary files … differ`, and `diff=<driver>` reads `diff.<driver>.binary`,
`xfuncname`, `funcname`, `textconv` and `command`. `text` and `-text` do not change patches. A file
is binary when either side is. Hunk-header function lines use the old side's pattern, else the new
side's, else Git's default rule. Patterns are POSIX expressions translated to `regex-automata`
syntax and matched leftmost-longest as in a UTF-8 locale; the first matching expression decides and
`!` rejects. `write_patch` refuses a driver with textconv or an external command
(`patch::External::Refuse`); `External::Ignore` matches `--no-textconv --no-ext-diff`. `Repo::blame`
refuses a textconv driver; `binary` and `-diff` do not affect `git blame`.

Git defines built-in drivers whose patterns are part of its source, which girt does not use. girt
has an original `rust` pattern written from the Rust grammar and checked against `git diff` output;
other built-in driver names (such as `python` or `cpp`) use Git's default rule unless configured
([issue 460](https://github.com/joshka/girt/issues/460)).
Known pattern differences: back-references, collating elements and equivalence classes are refused,
non-UTF-8 patterns are refused, named classes are ASCII, and a capture group inside an ambiguous
match follows preference order rather than POSIX subexpression rules. A C-locale Git matches invalid
UTF-8 bytes with `.`; girt follows UTF-8-locale Git.

### Diff Attributes Evidence

`src/attributes/tests.rs` covers each parsing and precedence rule with cases checked against
`git check-attr`. `tests/patch_attributes.rs` compares `Repo::write_patch` byte for byte with Git
2.56.0 `git diff` for forced text and binary, driver `binary`, `xfuncname` with groups and negation,
`funcname`, the last-configured pattern, the built-in `rust` pattern and its configuration override,
nested files, `info/attributes`, renames across drivers, committed-only attributes, `attr.tree`, a
bare repository and a symlinked `.gitattributes`. It also checks textconv refusal,
`External::Ignore` against `--no-textconv`, blame's textconv refusal and a tree source against
`git check-attr --source`. On rust-lang/rust 1.87.0..1.88.0, girt's patch matches `git diff -M` in
7,478 of 7,702 file sections (5,634 before attributes); no remaining difference is in function
context, and all remaining differences also occur with Git's attributes disabled. The built-in
`rust` pattern was derived from CLI observation only.

## Content Filters

`filter::Pipeline` converts content between its stored and working-tree forms
([issue 496](https://github.com/joshka/girt/issues/496)). `Repository::filters` loads the attribute
files Git reads and `core.autocrlf`, `core.eol` and `core.safecrlf`; `Pipeline::plan` resolves the
`text`, `eol` and `ident` attributes for one path.

Line endings follow Git: `-text` and `binary` never convert; `text`, or an `eol` attribute with
`text` unspecified, always converts; `text=auto`, or an unspecified `text` under `core.autocrlf`
`true` or `input`, converts only content that looks like text. Content is binary for `text=auto`
when it contains NUL, a CR not followed by LF, or more than one nonprintable byte per 128 printable
ones, where backspace, tab, form feed and escape are printable and a final DOS end-of-file byte is
not counted. Storing replaces CRLF with LF and keeps lone CR. Writing CRLF adds CR before each LF
that lacks one, except that `text=auto` leaves a blob with any CR unchanged. Under `text=auto` a
file whose staged blob already contains CR is stored unconverted, unless renormalizing. The `eol`
attribute picks the written line ending; otherwise `core.autocrlf=true` writes CRLF, `input` writes
LF, and `false` follows `core.eol`. `core.safecrlf` compares the working file with what writing the
stored result would produce: `warn` reports the change, `true` refuses, `false` is silent. `ident`
writes `$Id: <blob id> $` for `$Id$` and for `$Id:` text closed by `$` on the same line, and
collapses both forms when storing.

`filter=<name>` runs `filter.<name>.clean` before the line-ending step when storing and `smudge`
after it when writing, through `sh -c` in the working tree root with `%f` replaced by the
shell-quoted path and `%%` by `%`. `filter::Commands::Refuse`, the default, returns
`filter::Error::Refused` instead of running a command. A failing driver leaves content unchanged and
is reported in `Converted::skipped`, as Git prints an error and continues; with
`filter.<name>.required` a failure, or a missing command, is `filter::Error::Driver`, as Git exits
128. A driver name with no configuration leaves content unchanged. Output above
`filter::Limits::max_output_bytes` stops the command.

`filter.<name>.process` takes precedence over `clean` and `smudge`. The process starts on first use
and stays running for the `Pipeline`; girt sends Git's `git-filter-client` and `version=2`
handshake, offers `capability=clean` and `capability=smudge` (never `delay`), then for each file
sends `command=`, `pathname=` and, when writing, `blob=<id>`, followed by the content in pkt-lines
of at most 65,516 bytes. `status=error` (before or after the content) fails that file;
`status=abort` fails it and stops further requests in that direction. A process without the
capability for a direction leaves content unchanged, or fails a required driver. A broken stream
discards the process; the next request starts a new one. Paths with `working-tree-encoding` are
refused with `filter::Error::Unsupported`.

`Repository::status_with_filters` converts each tracked regular file it reads with
`Pipeline::to_repository`, without `core.safecrlf`, and hashes the result; `text=auto` reads the
staged blob only when its rule needs it. Files whose stat words match the index are trusted unread,
as before. As in Git, a file whose size differs from a nonzero cached index size is modified even
when its converted content matches the index (observed: changing only line endings under
`core.autocrlf=input` makes `git status` report `M` while `git diff` is empty). Workers read files
in parallel and convert them one at a time through the shared pipeline.

`Repository::checkout_tree_with_filters` converts every regular file of the baseline and target with
`Pipeline::to_worktree` during preparation, before any working-tree change, so a failing required
filter or a refused command changes nothing. The baseline is clean only when each file equals its
converted form, and publication verifies the converted bytes; symlinks are not converted. Paths a
failing non-required driver left unconverted are listed in `checkout::Report::unfiltered`. It takes
two pipelines: the baseline is verified with the attributes in the working tree
(`Repository::filters`), and the target is written with the target's
(`Repository::filters_with_source` with `DirectorySource::Tree`), as Git reads them from the index
it checks out. A file whose attributes change between the trees, such as `eol=lf` to `eol=crlf`, is
accepted as clean and rewritten.

`working-tree-encoding` converts the UTF-16 and UTF-32 families natively, after the clean driver
when storing and before the smudge driver when writing. `UTF-16` and `UTF-32` require a byte order
mark when storing and write one in the platform `iconv`'s order (big-endian on macOS, the machine's
order with glibc); the `LE`/`BE` forms refuse a leading mark; `UTF-16LE-BOM` accepts either and
writes one. Empty files stay empty. A file that is invalid in its encoding, or breaks the mark rule,
is refused with `filter::Error::Encoding`, as `git add` exits 128. A blob that is not valid UTF-8 is
written unencoded and flagged in `Converted::unencoded`, as Git reports an error and writes it.
Other encodings are refused ([issue 593](https://github.com/joshka/girt/issues/593)).

`workflow::clone` and `Repo::clone` check out through the cloned repository's filters, reading the
target tree's `.gitattributes` (or the tree `attr.tree` or `GIT_ATTR_SOURCE` names) and the
configuration the clone was opened with, as `git clone` does. `Repository::filters_for_checkout`
makes the same choice for `Repo::switch` and the CLI. `CloneOptions::filter_commands` defaults to
running filter programs; `None` writes raw blob bytes.

### Content Filters Evidence

`tests/filter_eol.rs` compares `Pipeline::to_repository` with the blobs `git add` stores and its
`safecrlf=warn` warnings, and `Pipeline::to_worktree` with the files `git checkout-index` writes,
for 8 `core.autocrlf` × `core.eol` combinations, 13 attribute lines and 11 content shapes (LF, CRLF,
mixed, lone CR, trailing CR, NUL, no final newline, empty, control bytes, DOS end-of-file and
`ident` keywords), on Git 2.56.0 with system and global configuration disabled. It also checks the
staged-CR rule and `--renormalize`, and `core.safecrlf=true` refusals by exit status. Unit tests in
`src/filter/eol.rs` pin the nonprintable-byte threshold measured with `git add` (1 per 128, 2 per
256), and `src/filter/ident.rs` the keyword forms observed with `git add` and `git checkout`.
`tests/filter_drivers.rs` compares clean and smudge output, `%f` and the working directory with
`git add` from a subdirectory and `git checkout-index`, including a path with a quote and space, a
failing driver, a driver combined with `text eol=crlf`, an unconfigured driver and a command that
reads only part of its input; it checks required failures against Git's exit status, refusal, and
the output limit. `tests/filter_process.rs` runs an original Perl process filter
(`tests/fixtures/filter-process.pl`) under `git add` and `git checkout-index` and compares both the
results and the request headers it logged, including content over one pkt-line, an empty file and
`status=error` before and after content; it checks `status=abort` against Git's exit status, and a
Git LFS round trip (pointer and content) against `git add` when `git-lfs` is installed (3.8.0 on the
development machine). `tests/filter_status.rs` compares `status_with_filters` with
`git --no-optional-locks status --porcelain` after aging the index so files are reread: CRLF files
checked out under `core.autocrlf=true` (clean, while raw status reports them), a line-ending-only
change (modified), `ident`, `text` added after a CRLF commit (modified), same-size edits through a
clean filter (clean when the cleaned content matches), refusal of filter commands, and Git LFS
files. `tests/filter_checkout.rs` checks out two commits with girt and with `git checkout` in twin
clones and compares every file byte for byte under four `core.autocrlf`/`core.eol` settings, with
`text`, `eol=crlf`, `text=auto`, `-text`, `ident`, a clean/smudge driver, a symlink and a removed
file; `git status` then reports girt's checkout clean. It also checks an attribute change between
the trees, refusal of a locally changed converted file, that refused commands leave the working tree
empty, and a Git LFS checkout. `tests/filter_encoding.rs` writes blobs with `git checkout-index` and
stores them back with `git add` for seven encodings alone and combined with `text eol=crlf`,
including non-BMP text and an empty file, and checks Git's refusals and the unencoded write of an
invalid blob. `tests/filter_clone.rs` compares a `workflow::clone_with` checkout byte for byte with
`git clone` under a global configuration with `core.autocrlf=true` and a smudge filter, and checks
the raw and refusing options and an `attr.tree` override.

## Working-Tree Index

`index::Index` provides bounded SHA-1/SHA-256 v2/v3/v4 parsing, construction, entry replacement and
encoding. `Version` selects framing through `Index::set_version` or the locked editor's
`set_version`. New indexes choose v3 when extended flags are present, otherwise v2. Entry edits
retain the version, upgrading v2 to v3 when necessary. Explicit conversion to v2 refuses extended
flags. V4 paths use checked prefix decompression and maximal shared prefixes when newly encoded.

`Entry` drafts preserve byte paths, canonical regular/executable/symlink/gitlink modes, object IDs,
raw stat words, stages 0–3, assume-valid, intent-to-add and skip-worktree. Construction sorts
unsigned path bytes then stages; parsing never repairs ordering. Both reject duplicate stages,
normal/conflict mixtures and file/directory collisions within a stage. Paths reject empty
components, NUL, `.`, `..` and `.git`; platform aliases and non-UTF-8 bytes remain data. Object
targets, stat correctness and worktree policy remain caller obligations. Existing raw
status/checkout workflows explicitly refuse intent-to-add and skip-worktree until consumer policy is
supplied.

Input/output bytes, entry count, per-path bytes and extension count have limits. Parsing verifies
the checksum before allocating entries and bounds decompressed path bytes and extension copies. It
retains original encoded bytes alongside decoded information to preserve alternate valid v4
compression and extended-flag framing. Storage may retain additional original/output buffers. These
are proportional bounds, not a precise heap budget. Zero/omitted checksums, unknown flag bits,
noncanonical modes and unknown mandatory extensions are refused. Split `link` dependencies resolve
against checksum-validated immutable shared files beside the per-worktree index. Main plus shared
encoded bytes share the input ceiling. Unchanged publication preserves the original main file; entry
or version edits deliberately publish a standalone full index without deleting shared files. Both
main and shared bytes are rechecked before publication. Missing/corrupt shared storage fails without
writing; no hidden retries or shared-file expiry are performed.

Sparse directory entries retain mode `040000`, trailing slash, stage zero and skip-worktree plus the
`sdir` marker. Entry edits preserve remaining sparse directories. `Index::expand_sparse` and
`IndexEdit::expand_sparse` explicitly replace all collapsed directories with skip-worktree leaves,
reading validated trees with aggregate tree/count/path/depth limits and cooperative cancellation.
Ordinary entries retain metadata; expanded leaves use zero stat words and inherited assume-valid.
Missing/wrong-kind/corrupt trees and failed validation leave the draft unchanged. Leaf targets are
not read. Empty directories disappear. Expansion neither matches sparse-checkout patterns nor
materializes files. These operations compose through `ColocationEdit::index_mut`; the caller still
owns staging, placeholder and materialization policy. See [R38 evidence](evidence/r38.md).

Optional payloads remain opaque. Unchanged indexes round-trip exactly. Changed entries or version
conversion discard `UNTR`, `FSMN`, `IEOT` and `EOIE` caches. Entry edits also discard `TREE`;
standalone version conversion retains its exact payload because entry semantics are unchanged.
Converting a split index conservatively discards `TREE` with its `link` representation. `REUC` bytes
remain because resolve-undo describes prior conflicts independently of current entries. Unknown
optional extensions block edits. Identical entry replacement retains every extension and original
byte. Failed edits leave the prior snapshot unchanged. Git-generated cache-tree, resolve-undo, split
and sparse fixtures test this boundary in both formats; see [R12 evidence](evidence/r12.md).

`IndexEdit::invalidate_entry_offsets` explicitly removes standalone `EOIE` and `IEOT` accelerators
without changing entries, stat data, flags, framing version or `TREE`/`REUC`/`sdir` payloads. Both
offset extensions are removed because canonical re-encoding may change entry byte boundaries. `FSMN`
and `UNTR` caches are also dropped. Other extensions, including split `link`, block this bounded
edit. Absent caches preserve the original encoding; failures preserve the draft. Optional cache
payloads may be malformed and are not interpreted, but normal outer framing, entry and checksum
validation still precedes editing. Publication remains explicit through `commit`.

`IndexEdit::discard_resolve_undo` explicitly removes `REUC` when the caller intends to forget old
conflict resolutions. It preserves entries, stat words, flags, version and `TREE` bytes, and retains
the original encoding when `REUC` is absent. Removing `REUC` also drops `FSMN`, `UNTR`, `EOIE` and
`IEOT` caches. Other extensions, including sparse `sdir` and split `link`, and output-limit failures
leave the draft and storage unchanged. Payloads are discarded opaquely; normal index framing and
checksum validation still applies. Entry replacement and tree-cache invalidation retain their
existing extension policies. Publication still requires `commit`.

`Repository::read_index_at` and `edit_index_at` accept an explicit standalone index path without
consulting `GIT_INDEX_FILE`. Relative paths resolve once against the process current directory;
publication appends `.lock` to the complete filename beside the selected index. No other index path
is touched. The same lock ownership, byte snapshot checks and explicit commit lifecycle apply. Both
explicit-path APIs reject every split `link` extension before reading dependencies, including a null
shared identity. Default index operations retain their existing split support.

Leaf symlinks and non-regular destinations are rejected; symlink ancestors are retained. This is a
conservative admission policy, not Git's override behavior. Independent disposable Git observations
show relative overrides based on the working directory and leaf symlinks followed for reads and
writes, with the target updated and the link retained. Public gix 0.87 observations instead replace
the leaf link when writing. Callers requiring either leaf-symlink behavior must select another path
or use their compatibility implementation before beginning a native edit.

`Repository::read_index` returns `None` for absence and `Some` for a valid empty index. The path is
always the resolved per-worktree `git_dir()/index`, including linked and separate Git directories;
`GIT_INDEX_FILE` and configuration overrides are ignored. `edit_index` creates `index.lock`
exclusively before reading and holds it until commit/drop. Callers derive drafts from that locked
snapshot. Commit validates/encodes before writing, compares original bytes/presence before and after
the lock write, closes the descriptor and renames the lock over the destination. A cooperating
writer cannot interleave. Exact-byte comparisons detect observed noncooperating changes, but do not
prevent a noncooperating race after the final check. Symlinks and non-regular index files are
rejected; hostile directory/lock replacement is outside the filesystem contract.

Publication zeroes the cached size of entries whose mtime is at or after the earlier of the previous
index's modification time and the time of writing. Other entry stat words remain exact, and the
published file keeps its normal filesystem modification time. This prevents an index rewrite from
making previously racy entries look clean to Git. The Git CLI same-stat regression checks that
modified content remains visible with mtime restored and ctime checks disabled. Callers supplying
new stat data still own its correctness. Publication uses same-directory rename and its
host/filesystem replacement semantics. No fsync, crash durability, shared-permission handling or
blanket platform guarantee is claimed. Validation, write and rename failures preserve original
destination bytes except for independent concurrent changes. Only the acquired lock is cleaned;
cleanup failures may leave that owned lock for manual recovery. Existing locks are never stolen.
Index ownership is independent of reference storage.

The implementation uses the public
[index format specification](https://git-scm.com/docs/index-format) and original fixtures generated
with `add`, `update-index --index-info`, `read-tree`, `write-tree`, `ls-files --debug`,
`ls-files --resolve-undo`, and worktree/separate-gitdir commands. No Git source or upstream tests
were used. Git-created stat/flag fields and byte paths are read independently; Git observes
girt-written modes, stages, flags and stat words and writes compatible tree objects. Malformed
framing, ordering, flags, extensions, resource boundaries and injected write/publication failures
have focused unit coverage. The portable integration suite is explicitly selected for Windows; R12
records the focused native execution separately. No new dependency was added. Status, staging
policy, checkout, filters/attributes, merge resolution and filesystem scanning remain outside this
slice.

## Raw Working-Tree Status

`Repository::raw_status` compares HEAD (including unborn/detached HEAD) or an explicit tree with
this worktree's index, then verifies working files against stage-zero entries. Staged results reuse
`TreeChange`; unstaged results distinguish content/mode changes, missing paths and obstructions.
Conflicts retain their exact index stages. Missing index storage means an empty index, not an empty
baseline tree. As in Git, index blobs are not read: status compares identities, so a missing or
damaged index blob is fsck's concern. Without conflicts, the index's own trees are computed and
baseline subtrees with the same identity are skipped unread. Baseline leaf targets and submodule
commit targets are not resolved.

A working file is not read when the `lstat` taken during traversal matches every cached index stat
word (size, mtime and ctime with nanoseconds, device, inode, uid and gid) and the entry's mode.
Racily clean entries are read: those whose cached mtime or ctime falls in or after the second the
index file was last modified. Git's rule compares only the mtime, with nanoseconds, so girt rereads
a superset of the entries Git rereads. A zero cached size (Git's smudge) and a split index always
force a read. `tests/status.rs::matching_stat_words_are_trusted_unless_racy` compares each case with
`git status`, using an index whose cached identity deliberately disagrees with the file.

The API and every report explicitly select literal bytes and POSIX modes. Attributes, clean filters,
EOL conversion, ignore rules, `core.filemode`, `core.symlinks`, global config and environmental
normalization policy are not applied. Thus raw status can differ from Git status for a CRLF working
file with `text eol=lf`, an ignored file, or an assume-valid entry. Assume-valid is deliberately
ignored: such an entry is treated like any other, so a changed file whose stat words differ is read.
No hooks or filter commands run. Consumers must own normalization policy if they require Git-default
status.

Untracked policy is either `Omit` or `RawFilesWithoutIgnores`. The latter lists every encountered
untracked leaf, including ignored and special files, without reading its content; empty directories
are omitted. Both policies enumerate visited directory names for exact byte matching. Gitlinks are
always reported as unchecked and never traversed. Nested `.git` markers and the conservative bare
repository marker combination `HEAD`/`objects`/`refs` stop traversal and appear as boundaries. The
root `.git` marker is excluded silently. Known Git/common/object directories are excluded by
filesystem identity, including a separate Git directory located inside the worktree.

Filesystem traversal uses existing rustix descriptor-relative no-follow operations on macOS/Linux;
no dependency was added. Unsafe index components and metadata aliases are rejected before platform
path conversion. Symlink leaves are compared using target bytes, never followed. Symlink or file
ancestors, directories replacing files and special tracked files are explicit obstructions. Exact
name matching prevents case-insensitive lookup from making a differently spelled index path clean.
Linux preserves non-UTF-8 filename bytes. Colons and backslashes are ordinary name bytes, as they
are to Git on these platforms. Both platforms reject NUL, empty/dot/parent components and
case-insensitive `.git` components. Non-ASCII names are compared as bytes on both platforms. Other
platforms reject traversal before storage access; no Windows worktree-status support is claimed.

Limits cover index reads, baseline tree flattening, pack snapshots, object reconstruction and total
index/HEAD payload bytes, worktree file/total bytes, visited directory entries, generated paths and
retained ancestor prefixes. Directory depth is capped at 128 even with a larger caller limit;
symlink target reads additionally cap their buffer at one MiB plus one truncation-detection byte.
Cancellation is checked between synchronous phases/entries and file-read chunks. Reference
resolution retains its existing 32-hop selection and unbounded metadata-byte contract. Repository
opening and metadata/object trust assumptions are inherited; these are not process-wide memory,
wall-clock or hostile repository-storage guarantees.

Identity, mode, size, mtime and ctime observations are checked around file reads and directory
traversal. HEAD and index are reread before return. An observed change is an error without a partial
report. This is not an atomic snapshot: later changes, ABA replacement and changes invisible to
filesystem timestamps may escape detection. Directory descriptors pin inspected directories if
concurrently moved. Mount manipulation, hostile hardlinks and metadata replacement remain outside
the trust boundary. Reads may update access times; girt performs no writes or index refresh. A
report must never serve as authorization to overwrite files during future checkout work.

Original unit fixtures cover byte/content and mode changes, restored-mtime edits, symlinks, socket
and ancestor obstructions, exact-case lookup, metadata containment, resource limits, cancellation
and deterministic concurrent file/directory/HEAD/index changes. `tests/status.rs` independently
generates Git fixtures with isolated configuration, no rename detection, no optional index refresh
and explicit native mode/symlink behavior. It covers staged/unstaged combinations, conflicts,
unborn/missing index, loose/packed objects, raw normalization/ignore differences, linked/separate
worktrees and absence of writes. No upstream implementation or test source was used.
`tests/status_portable.rs` separately checks cancellation and explicit platform rejection; Windows
CI selects it deliberately. Native Linux/Windows execution of this increment remains pending.

The 2026-09-24 macOS arm64 run at `9affad69fac53f2c022f47f46b40f8ee7cc588af` used Rust/Cargo 1.98.1
and Git 2.55.0. Full checks passed 1,146 units, 507 integrations and 19 doctests, including 37 local
status cases and 35 status integration cases. See the
[baseline](benchmarks.md#raw-working-tree-status-baseline) for representative warm-storage
measurements. No publication, merge or checkout is part of this increment.

### Ignore-Aware Status

`Repository::status` applies index flags and `core.fileMode` while comparing literal content bytes,
and lists untracked paths through Git's ignore sources. `Repository::ignore_rules` reads
`core.excludesFile` (with `~/` expanded from the supplied `HOME` and a relative path resolved
against the worktree, as Git resolves it) or the `$XDG_CONFIG_HOME/git/ignore` default, then
`info/exclude`; `core.ignoreCase` selects ASCII case folding. Status reads each visited directory's
`.gitignore` through the traversal's no-follow descriptors. As in Git, a `.gitignore` symlink is
skipped with no rules applied, tracked paths are never ignored, an ignored directory hides every
untracked descendant, and only regular files, symlinks and nested repositories are listed.
`UntrackedFiles::All` matches `git status --untracked-files=all`; `UntrackedFiles::Collapsed`
matches the default listing, including Git's omission of a directory that occupies a tracked file's
path.

Ignored directories are not entered unless they hold tracked paths, so their contents consume no
directory-entry budget. `~user/` excludes paths are refused, where Git expands them.

`tests/status_ignore.rs` compares both listings with `git status --porcelain=v1 -z` on original
fixtures: negation, excluded parents, nested precedence, anchored and directory-only patterns, `**`,
tracked ignored paths, obstructions, nested repositories, FIFOs, symlinked `.gitignore`, every
configuration source and both `core.ignoreCase` values. It also compares `check-ignore -v`
provenance. Git 2.56.0 on macOS arm64 was the oracle.

## Raw Tree Checkout

`Repository::checkout_tree_with(baseline, target, limits, cancel)` materializes a selected SHA-1
tree and publishes its index on macOS/Linux. This is tree checkout: HEAD, refs, reflogs and
configuration stay unchanged. Callers supply the baseline tree rather than asking checkout to infer
intent from HEAD. Every normal index entry must match that baseline by path, mode and ID. Staged
differences relative to it, conflicts, gitlinks, dirty or missing tracked files and unsupported
index extensions are refused before worktree mutation, even for paths unchanged in the target. A
previous raw status report never substitutes for these independent checks.

`None` denotes an empty tree. A missing or empty index requires an empty baseline, which supports
unborn repositories and initial checkout after either girt or Git no-checkout clone. A missing index
with a nonempty baseline is rejected. An existing nonempty index cannot be bypassed by choosing an
empty baseline. HEAD is not read or validated; after checkout of a different tree, Git can report
staged changes relative to HEAD. Unverified `TREE` caches are discarded, including when entries are
unchanged. New index entries have zero cached stat words and cleared assume-valid flags. Git status
verifies their content; stat-only plumbing such as `git diff-files` may first need
`git update-index --refresh`.

Checkout preserves literal blob bytes, owner-executable mode and native symlink targets. It does not
run filters, apply attributes/EOL conversion, consult ignores or honor
`core.filemode`/`core.symlinks` overrides. Files receive 0644 or 0755 permissions; prior ownership,
ACLs and other metadata are not preserved. Symlink payloads must contain 1 through 1024 non-NUL
bytes. Empty-tree input is supported; empty directories encoded in trees are not materialized
because Git's index records only leaves.

### Preconditions and Filesystem Boundaries

Callers must exclude other worktree writers and root/ancestor renames throughout the operation, and
protect metadata, objects and mount topology from replacement. The per-worktree index lock excludes
cooperating index writers. Component-wise descriptor-relative opens never follow symlink ancestors;
regular-file writes use exclusively created temporary files, and exclusive `linkat` installation
refuses newly appeared destinations. Symlinks are installed as links themselves, never followed.
File identity and content are checked independently; path/ancestor identity is checked again at
mutation boundaries. Final target verification precedes index publication. These mechanisms detect
observed changes but cannot prevent an arbitrary writer racing the last unlink check, directory
rename or ABA replacement. Hostile hardlink/mount manipulation is outside the contract.

Linux preserves non-UTF-8 names. Name equivalence follows the working tree's volume, observed with
Git 2.56.0 on a default (case-insensitive) APFS volume and on a case-sensitive APFS disk image.
`git init` and `git clone` write `core.ignorecase=true` only on the first, and
`core.precomposeunicode=true` on both. On the case-sensitive image `xt_CONNMARK.h` and
`xt_connmark.h` both check out, while composed and decomposed `café` still collide: APFS folds
normalization regardless of case sensitivity. Girt folds case when `core.ignoreCase` is true or the
working tree's root resolves `.GIT` to its `.git` entry (a read-only probe of the volume being
written, not of the git directory, which may live elsewhere), and folds normalization on macOS only;
Git reads `core.precomposeUnicode` only on macOS, so a Linux volume keeps both spellings whatever
the setting. Config only adds folding to what the probe shows, so stale config cannot let two names
that collide on disk both be written. Where paths collide, Git takes them in index order: a file
keeps the last spelling with its content, a directory keeps the first and later spellings' children
are written into it, and `git status` then reports each shadowed file as modified when its content
differs. Girt makes the same choice and lists each set in `Report::collisions`.
`colliding_paths_check_out_like_git` compares files, index and status with
`git read-tree --reset -u` on the running platform. Girt still refuses a file and a directory
sharing a name, a spelling change of one name between baseline and target (a case-only rename on a
folding volume, [issue 620](https://github.com/joshka/girt/issues/620)), and an existing entry the
volume merges with a wanted name; Git handles the first two. Both reject filesystem name aliases,
NUL, empty/dot/parent components, case-insensitive `.git` components and components longer than 255
bytes. Colons and backslashes are accepted (`posix_colon_and_backslash_names_check_out_like_git`
compares the result with Git status). Metadata-name aliases follow Git's `core.protectNTFS` and
`core.protectHFS`, read from config at Git's observed defaults: NTFS protection on unless set to
false, refusing `git~1` and `.git` with trailing dots, spaces or a `:stream`; HFS+ protection on by
default on macOS only, refusing `.git` spelled with HFS-ignorable code points. Git 2.56.0 on macOS
refused `git~1` with neither option set, although its manual describes NTFS protection as a Windows
default. `metadata_name_refusals_match_git` checks every name under each setting against
`git read-tree --reset -u` on the running platform. Nested ordinary/bare repositories and
metadata-directory identities block traversal. Preparation projects marker-name presence through the
actual planned operation order, including retained siblings and directories left empty by tracked
deletions. It refuses any planned non-root directory with all three `HEAD`, `objects` and `refs`
names, conservatively including ASCII case aliases. Entry types do not narrow this predicate: even
three ordinary files are unsupported, although Git accepts that tree. This preflight prevents
checkout from activating its own live guard midway through mutation; it does not disable guards on
existing or newly created directories. Gitlinks, submodule recursion, sparse/split indexes,
intent-to-add/skip-worktree entries and resolve-undo and unknown extensions are unsupported. Derived
`TREE`, `FSMN`, `UNTR`, `EOIE` and `IEOT` caches are discarded even on unchanged checkouts; Git can
rebuild them. Full v2/v3/v4 framing is accepted. Windows and other platforms refuse checkout before
locking or reading the index; portable tests cover that boundary without claiming Windows
materialization support. Native hard links, symlinks and POSIX modes are required; filesystem
emulation and shared-permission policies are excluded.

### Phases and Failure Recovery

Preparation acquires the index lock, verifies the baseline/target trees and blobs, checks clean
tracked content and obstructions, and validates the complete replacement index. Bounds cover tree
traversal, unique retained blob payloads, files, path prefixes, depth and directory enumerations.
Directory names are enumerated again at mutation boundaries, so wide directories can require
quadratic work. Cancellation is checked between entries and 64-KiB file I/O chunks; one syscall or
object decode cannot be interrupted. There is no wall-clock deadline or background worker.

Mutation removes changed tracked leaves, removes known empty directories only when necessary for a
directory-to-file transition, creates missing directories, then installs changed leaves. Unknown
contents, including empty subdirectories, obstruct transitions; no recursive cleanup or force mode
is provided. Unrelated files and directories are retained. Publication rechecks all target content
and original index bytes, then writes and renames the owned index lock. The index uses its existing
conservative timestamp policy. There is no batch atomicity, crash durability or rollback journal.

A failure reports the last entered stage, every completed namespace operation in order, whether the
index was published, and separate cleanup failures identifying owned artifacts that may remain.
Multiple operations can name one path. A post-mutation inspection failure still reports that
mutation. If index publication fails, successful worktree changes remain with the prior index,
except for independent writers. Cleanup checks temporary identity and refuses observed replacements;
it never recursively deletes a directory. Explicit index abort reports lock cleanup failures. Drop
remains best effort for callers that abandon an index edit without explicit abort.

Recovery requires inspecting both the report and actual files. Do not blindly retry with the old
baseline or remove reported artifacts without verifying ownership and excluding writers. A failed
update may leave tracked paths absent or changed and newly created empty directories. A successful
return means the selected tree and index were verified and published under the exclusion contract,
not that another writer cannot change them afterward.

### Checkout Evidence and Provenance

Original local tests exercise initial checkout, additions/deletions, modes, symlinks and tracked
file/directory transitions; staged/conflicted/dirty states; unsafe paths, aliases, gitlinks and
nested repositories; object corruption and bounds. Deterministic checkpoints cover changed files,
symlink/directory/root substitution, newly appeared destinations, cancellation, write/install/delete
faults, post-mutation failures, cleanup failures and index-publication preconditions. Index storage
units separately inject descriptor-write and rename failures and verify original-byte preservation.

Independent Git CLI fixtures create their own commits and compare `ls-files --stage`, `write-tree`,
status, raw objects and unchanged HEAD. Both girt and Git no-checkout clones become usable
checkouts, then accept subsequent Git commits and `fsck`. Linked and separate worktrees route the
index correctly. An attributes fixture proves the deliberate difference between raw LF
materialization and Git's CRLF checkout conversion. No upstream implementation or test source was
used as input.

Run `cargo run --example checkout` for a disposable public lifecycle. The
[benchmark workload](benchmarks.md#raw-tree-checkout-baseline) records the measured scope.

## Colocation and Operation Metadata

`Repository::edit_colocation` locks the per-worktree index before reading it. Callers derive entry
changes through `ColocationEdit::index_mut`; `IndexEdit::replace_entries_reusing_stat` copies cached
stat words only when path, stage, object ID, mode and all flags match the locked draft. Other
entries retain caller-supplied stat words. Existing extension invalidation and conservative
racy-stat publication rules apply. Intent-to-add and skip-worktree are explicit draft data: girt
neither creates working files nor decides which placeholders or sparse entries jj should retain.
jj's merged-tree staging, reset and materialization policies stay with jj. These primitives do not
invoke raw status or checkout, whose extended-flag refusal remains a separate contract.

`ColocationEdit::commit` prepares and locks a conditional stored HEAD update before publishing the
index, then publishes HEAD under those reference locks. A direct target detaches without updating
the previous branch; a symbolic target may name an unborn branch but does not delete an existing
branch. Expected-old and reflog policy are explicit. Preparation failure changes neither file; index
failure preserves its old bytes and does not attempt HEAD publication. A later HEAD failure reports
that the index is published and retains reference/reflog outcomes. The two publications are not
atomic together and are never rolled back. Callers needing another order can compose the existing
index and reference primitives directly.

`Repository::operation_state` snapshots the recognized merge, cherry-pick/revert, rebase and
sequencer metadata in the worktree Git directory, including linked layouts. It also captures
AUTO_MERGE, REBASE_HEAD and BISECT_LOG. The latter is a colocation cleanup artifact, not complete
bisect-reset support. Inspection retains opaque bytes and simultaneous roots without inferring a
single command or parsing payloads. Unknown external roots, ORIG_HEAD, refs, logs, index and working
files are preserved. Regular descendants of recognized operation directories belong to the snapshot;
symlinks, special nodes and unexpected root types are refused. Byte, node and depth limits bound
inspection; depth has an additional hard cap of 128.

`OperationState::cleanup` rechecks the complete snapshot before deleting children before parents.
Preflight failure deletes nothing. Later failure returns exact removed paths and the underlying
cause; callers inspect again before recovery. Git supplies no common operation-state lock: callers
must exclude operation writers throughout inspection and cleanup and keep metadata directories
trusted and stable. Rechecking detects observed changes but cannot exclude later writers or ABA
replacement. Cleanup discards metadata only and does not abort/reset working files. No crash
recovery or durability guarantee is added. Independent fixtures and limitations are recorded in
[R13 evidence](evidence/r13.md).

## R14 External Storage Discovery

R14 extends SHA-1/SHA-256 local pack reads to pack v3 and index v1. Writers still emit pack v2 and
index v2; transport import version support is unchanged. Legacy indexes have no CRC table, so
explicit whole-pack/index checksum verification and read-time object identity verification supply
integrity checks. See [R14 evidence](evidence/r14.md) for the independent Git/jj matrix and
remaining capability owners. Historical pack records above describe their original revisions, not
current limitations.

## R15 File-Backed Storage

R15 replaces whole-pack retention with pinned pack/index files and bounded index offset tables.
`PackLimits` now distinguishes aggregate validation work, index input, file handles and directory
enumeration. Index identities stay on disk; pack validation and decoding stream through bounded
buffers. `Objects::clone` shares handles/tables. There is no retained decoded-object cache or
negative cache. The default artifact budget is 64 GiB on 64-bit hosts, while memory and handles have
independent limits. See [R15 evidence](evidence/r15.md) for exact defaults, original large fixtures,
measurements and native coverage.

`Repository::objects_controlled` and `Objects::read_controlled` add cooperative cancellation.
Filesystem calls remain blocking. Pinned files must remain immutable; in-place writes are outside
the consistency contract. [R16](#r16-object-store-refresh) supplies explicit refresh and
retained-reader consistency contracts. Earlier reports of owned pack bytes describe the historical
implementation.

## R16 Object-Store Refresh

`Objects::refresh(PackLimits, AlternateLimits)` rediscovers the alternate graph from the original
canonical primary directory and validates every indexed pair. `refresh_controlled` adds cooperative
cancellation. Success replaces this reader's pack/topology view; failure releases the candidate view
and preserves the existing one. No files are changed. Shallow boundaries remain inherited from the
repository handle; refreshing objects does not refresh references or shallow metadata.

Cloned readers keep their previous topology and Arc-shared pinned artifacts. Rename, unlink and path
replacement preserve access to those artifacts where the OS permits the operation. Writers must keep
the opened files immutable: truncation and in-place rewriting are unsupported. Loose objects remain
live at stored canonical paths, so old readers can observe new loose objects or lose loose-only
objects after GC. Removing an alternate record affects refreshed readers; retained readers still
search that alternate. Removing or replacing the directory itself can change its live loose
contents, while its pinned packs retain the old files. There is no GC lease or repository-wide
point-in-time snapshot.

Publish complete immutable packs before their complete indexes, using atomic final-path publication.
An unindexed pack is ignored. An index without its pack, a mismatched pair, or corrupt bytes is an
error. A successfully pinned pair may have been removed from the directory by the time refresh
returns. Concurrent new publication may require another refresh to become visible. Readers never
refresh on a miss and keep no negative cache; concurrent loose publication needs no refresh.

Every refresh makes exactly one discovery/validation attempt, with no automatic retries or sleeps.
Missing-path races, corruption, resource exhaustion and cancellation remain distinct errors. After
publication finishes, or after repairing/removing broken artifacts, callers may retry explicitly
under their own finite attempt budget. A failed refresh can continue serving its previous pinned
packs; it cannot promise that live loose objects remain available. Exclude publishers when a single
consistent topology is required.

Refresh revalidates every pair, including unchanged names. Its cost is a full opening scan. During
refresh, current and candidate views coexist: aggregate handles and tables may reach the sum of both
generations' limits. Further retained generations multiply that cost; clones within one generation
share handles and tables. R15's bounded per-generation accounting and per-read work limits still
apply. [R16 evidence](evidence/r16.md) records publication, recovery and native cases.

## Caller-Controlled Interrupt Lock Cleanup

`girt::locks::cleanup` releases this process's registered transient lock files during normal-context
shutdown. It includes index/config edits, files/reflog/packed-ref/reftable guards, shallow metadata,
credentials, clone config and worktree administration mutexes. It does not scan directories or
recover foreign stale locks. Persistent worktree `locked` markers and `.keep` protections are
preserved.

jj owns signal handling. A callback thread must request cancellation, stop admitting work, wake
writers, and invoke cleanup before joining any pending uncancellable packing wait. Cleanup cancels
pending reference lock waits, including packing's wait. A raw POSIX signal trampoline must delegate
this work to normal context: the API allocates, locks and performs filesystem I/O. An invocation
inside a girt lock callback returns `WouldBlock`; it cannot wait on its own operation. Active
synchronous filesystem operations can delay cleanup without a deadline.

Cleanup and active acquisition/publication/drop are mutually exclusive. Surviving Unix guards are
invalidated before their identity-matched paths are unlinked, and Windows guards once their file is
deleted; they cannot publish through a new writer's replacement lock. Completed
ref/index/config/worktree effects remain completed. Worktree repair retains its administration lock
across both link replacements, and colocation retains its index/HEAD operation gate across both
publications. New work after cleanup can acquire locks, so shutdown admission control remains the
caller's responsibility.

Report entries identify removed/absent, replaced/preserved and failed paths. Unix invalidated
failures are not retried by later cleanup calls; preserve the first report for manual recovery.
Paths are resolved before creation, and registration is infallible after identity capture. An
unavailable acquired-file identity refuses cleanup and names the lock requiring manual recovery in
the acquiring operation's error. Writer and identity pin share one OS handle. Unpublished temporary
files and crash durability are outside this lock-only API.

Unix registrations pin the file inode and compare device/inode before unlinking. Identity checks
cannot protect a check/unlink pair against non-cooperating lock-path or parent-directory
replacement; callers must exclude those races.

Windows lock handles carry delete access without changing their sharing, and registrations record
the volume and 128-bit file ID. Cleanup opens the path for attributes only, which conflicts with no
sharing mode, and acts only when it names the registered regular file. Every change then goes
through a handle to that file. A deletion through an open handle completes only when the handle
closes, so while the writer is open cleanup renames the file through the writer's handle to a unique
`<name>.girt-released-*.lock` name beside it and marks it for deletion. The registered name is free
at once, so Git can take the lock while a retained edit still holds its handle. The renamed file
disappears when that handle closes, including at process exit, and Git ignores `.lock` names in the
meantime. Once the writer has closed, cleanup opens the path with delete access, checks the identity
again and deletes it. Ref, reflog, reftable and shallow locks deny delete sharing, so their path
cannot change between check and rename. Index, config, clone-config, credential and worktree-admin
locks share delete for rename publication and have the same non-cooperating-writer limit as Unix. A
failed Windows rename or delete is reported with its path and keeps the guard valid for normal
Drop/abort. If the deletion mark fails after the rename, the name is already free, so the guard is
invalidated and the failure names the moved file for manual removal. Git for Windows terminated
while holding `index.lock` leaves it in place, and cleanup never touches unregistered locks.

Other platforms report `Unsupported` for existing registered paths, preserving both the lock and its
guard's validity for normal Drop/abort. Already absent paths do not require removal. No library
signal handlers or new variants in existing public error enums are introduced.

## Content Merging

`tests/merge_content.rs` compares original byte fixtures with the installed `git merge-file` under
isolated system/global configuration. It checks clean separated changes, adjacent and overlapping
changes, identical replacements, unchanged sides, empty inputs, deletion, invalid UTF-8, missing
final LF and common boundary lines. Merge, diff3 and zdiff3 marker output and ours/theirs favoring
are compared byte for byte, including clean/conflicted exit classification. Union is compared with
Git's default merge-style refinement; its result is independent of subsequent marker presentation.
Explicit binary detection/favoring, custom marker labels/width, cancellation and input bounds have
focused checks.

The implementation uses bounded Myers comparisons and connected base edit intervals. It does not
claim Git's repeated-line placement or internal common-line conflict refinement; those are tracked
in [the refinement task](https://github.com/joshka/girt/issues/498). Tree merging, renames, virtual
bases, attributes, configured whitespace and index/working-file publication are not implemented by
this increment. See the [content merge guide](merging.md).

## Tree Entry Merging

`tests/merge_tree.rs` generates original SHA-1/SHA-256 trees and histories through Git. Clean
content/mode combinations and nested additions/deletions match `git merge-tree --write-tree` tree
identities. Content, add/add and modify/delete conflicts preserve the same original stage entries as
Git's merge-tree output. Focused fixtures cover symbolic links, type/directory conflicts, unresolved
gitlinks without readable submodule commits, object-store side-effect absence, cancellation and
traversal entry limits. All Git calls isolate system/global configuration.

`Objects::merge_tree_entries` is explicitly path-based and does not infer renames or directory
renames. Directory conflicts remain structured entries and cannot yet become leaf index stages. The
provisional conflicted tree selects ours (or the surviving side for modify/delete), rather than
Git's marker blobs and conflict-path renaming. Generated objects remain in memory until the caller
installs them. Virtual merge bases, driver selection and publication are separate layers.

## Merge Driver Selection

`tests/merge_drivers.rs` compares set, unset, unspecified and named merge attributes with isolated
`git check-attr --cached`, including built-in text/binary/union and a custom name. Focused checks
cover effective conflict-style precedence, invalid styles, configured default-driver selection,
explicit custom dispatch and unresolved binary input under text/union. Original isolated
`git merge-tree --write-tree` observations establish that text and union retain binary conflicts;
`git merge-file` observations establish case-sensitive conflict-style values.

Custom drivers are returned to the caller, not executed implicitly. Configuration and attributes are
explicit input snapshots; tree-path routing and configured external command execution remain
caller-owned composition.

## Rename-Aware Tree Merging

`tests/merge_renames.rs` compares independently generated exact and similarity rename/modification
histories with isolated `git merge-tree --write-tree`. Clean SHA-1/SHA-256 tree identities match.
Rename/rename conflicts preserve Git's distinct stage paths; rename/delete and directory-rename
addition fixtures match conflicted candidate tree identities. Explicit refusals cover destination
collisions and stored empty subdirectories.

The normalization layer uses existing rewrite inference and refuses skipped inexact search. Removed
directories with multiple inferred destinations and destination collisions return typed errors.
Stored non-root empty trees require the path-based primitive instead. This increment does not claim
complete ort rename behavior for those layouts.

## Commit Merging and Virtual Bases

`tests/merge_commits.rs` creates original ordinary and criss-cross graphs through isolated Git
commands. Ordinary SHA-1/SHA-256 clean results and a two-base criss-cross merge with final rename
inference match `git merge-tree --write-tree` identities. The criss-cross fixture requires an
unstored virtual content blob for rename scoring and verifies that computation never installs it.
Focused tests cover explicit unrelated-history policy, cancellation and unresolved-base refusal.

Virtual bases fold deterministic object-ID-ordered sets using existing best-common-ancestor queries.
Unresolved virtual bases remain an explicit typed error with their intermediate conflict data;
conflict-bearing recursive-base compatibility is a separate increment. The rename-layout and
content-refinement limitations above still apply. No reference or merge commit is implicitly
published.

## Merge Conflict Publication

`tests/merge_publication.rs` generates original trees in disposable Git repositories, writes
conflict files through guarded checkout and compares `git ls-files -u` with the prepared original
stage paths, modes and IDs. Marker bytes are checked exactly. Preservation cases cover dirty tracked
content/index bytes, untracked obstructions and foreign index locks. Pre-publication cancellation
preserves objects and working content; edited generated blob identities and marker output exhaustion
are refused during preparation.

Publication reuses the checkout mutation engine and its no-follow paths, baseline/target
verification, held index guard, cancellation and partial reports. The staged replacement is
validated under that guard before working-file mutation and published last. Existing checkout
fault/race tests cover the shared engine. The same Linux/macOS/path/gitlink restrictions and
caller-owned writer exclusion apply; no batch rollback or crash durability is added. Directory
conflicts require explicit leaf expansion before publication. Resolve-undo remains index codec data;
this increment does not add rerere or automatic conflict resolution.

## Merge Whitespace Policies

`tests/merge_whitespace.rs` compares original histories with isolated `git merge-tree --write-tree`
and its ignore-space-change, ignore-all-space, ignore-space-at-eol and ignore-cr-at-eol strategy
options. It checks clean tree identities and conflict classification, whole-side whitespace-only
selection, substantive changes with formatting differences, retained context, insertion/deletion
mapping, CRLF, invalid UTF-8 and missing final LF. Work exhaustion and pre-normalization
cancellation have focused checks. The existing exact-byte content oracle matrix remains applicable.

Matching uses comparison-only line keys and maps edits back to original byte ranges. Missing final
LF remains distinct even for a line whose normalized body is empty. Repeated-line placement and
internal conflict-refinement limits remain tracked in the content-refinement issue.

## Per-path Merge Drivers

`tests/merge_path_drivers.rs` compares union, binary, unset and undefined custom merge attributes
with isolated `git merge-tree --write-tree`. The bare oracle receives the same explicit attribute
rules through `info/attributes`; no working-tree attribute lookup is implied. Tests also cover
destination paths after similarity renames, caller-provided resolved bytes without object
publication, retained callback error sources, cancellation after callbacks, and one mutable driver
shared by recursive virtual-base and final merges.

Driver selection remains caller policy: attribute/configuration snapshots and any custom process
execution are explicit. Existing content refinement, rename normalization and conflicted virtual
base limitations remain. Directory conflict publication still requires leaf-stage expansion, tracked
in [issue 575](https://github.com/joshka/girt/issues/575).

Add/add regular files with differing executable modes still compare content independently. SHA-1 and
SHA-256 oracle cases compare prepared marker-tree identities with Git and retain original
stage-two/stage-three modes. A focused assertion confirms generated recursive-base trees remain
available in the returned object collection.

## Commit replay

`Objects::cherry_pick` and `Objects::revert` compare explicit source/selected-parent/destination
snapshots with the rename-aware merge engine. `tests/replay_git.rs` generates independent Git CLI
fixtures with isolated configuration and fixed identities and dates. SHA-1 and SHA-256 cherry-pick,
provenance (`-x`) and ordinary revert produce identical commit identities. Root revert and merge
mainline selection are covered, along with original conflict sides and absence of state publication.
Originally empty commits (`--allow-empty`) and redundant changes use separate stop/keep/skip
policies; keeping both corresponds to `--keep-redundant-commits`.

Computation creates no objects, index, working files, state files or references. Source author is
preserved for cherry-pick; caller identity supplies the new committer and both revert identities.
Opaque source headers and signatures are omitted. Messages are byte-oriented; caller-supplied
messages are not cleaned through Git configuration. Existing content/rename merge limitations apply.
Source messages are retained without Git's configurable cleanup or encoding conversion;
configuration-specific trailer tokens remain caller policy. Linear todo execution and explicit
sequencer metadata publication are described below. Saved stash objects and application have their
own sections below.

### In-memory rebase

`Objects::rebase` replays an explicitly ordered sequence onto a supplied destination, retaining
intermediate objects in memory. The Git fixtures compare two-commit sequence identities with
`git rebase --onto` in SHA-1 and SHA-256 repositories. Additional cases cover first-conflict
progress, redundant skipping, input bounds, cancellation and no intermediate object publication.
Automatic range selection, patch-ID filtering, merge-topology reconstruction and on-disk state are
not part of this primitive.

### Interactive todo plans

The todo parser accepts Git's long/short pick, reword, edit, squash, fixup, drop, exec, break,
label, reset, merge and update-ref syntax, including fixup/merge message flags. Linear execution
retains source authors, combines fixup/squash metadata and returns explicit update-ref intents.
Message editing and external execution require caller hooks. Merge/reset topology is refused before
hooks; [generated-history merge lookup](https://github.com/joshka/girt/issues/616) tracks the
missing ancestry boundary. Todo stops return progress; persistent continuation is caller-owned.

`tests/replay_git.rs` compares interactive fixup and autosquash-amend identities with Git and covers
provenance spacing beside trailer footers, custom content drivers and conditional ref planning.
Explicit update-ref targets retain their instruction position even when a later fixup replaces that
commit. `noop` is accepted. Parsing uses `#` comments; custom Git comment markers require an
explicit caller policy and are tracked with
[sequencer option snapshots](https://github.com/joshka/girt/issues/621).

### Git sequencer metadata

`sequencer::State` reads single-operation pseudo-refs, multi-commit cherry-pick/revert directories
and merge-backend rebase metadata. Recovery plans separate object/checkout/ref publication from
explicit state-file publication. Cherry-pick retains its current todo row while stopped; rebase has
already moved that row to `done`. Continue/skip plans account for that distinction. Abort retains
`ORIG_HEAD`; rebase finish exposes conditional branch/update-ref intents before metadata removal.

Metadata publication requires caller-owned exclusive operation access. Changed snapshots and lock
conflicts fail before replacement; later failures report paths already changed. Unknown files,
symlinks, apply-backend state, sequencer option files and squash/fixup continuation state are
explicit refusals. Source author scripts are preserved as bytes and never executed. Merge/reset
topology is tracked in [the generated-history follow-up](https://github.com/joshka/girt/issues/616).

`tests/sequencer_git.rs` exercises Git continuing girt-created stops/todos, girt resolving a Git
rebase stop, explicit skip/abort, continuation reflogs, stale snapshots, lock conflicts,
unknown-file preservation and symlink refusal. Complete state directories are retired by rename
before teardown cleanup. Cleanup failure can leave inert metadata at the reported retired path,
while Git sees no active operation; it cannot leave a partly deleted active directory.

## Pathspecs

[`girt::pathspec`](../src/pathspec.rs) parses and matches Git pathspecs: plain paths with
component-boundary directory matching, `fnmatch` wildcards where `*` crosses `/`, and the `top`
(`:/`), `exclude` (`:!`, `:^`), `icase`, `literal`, `glob` and `attr:` magic in long and short
forms. The `GIT_GLOB_PATHSPECS`, `GIT_NOGLOB_PATHSPECS`, `GIT_ICASE_PATHSPECS` and
`GIT_LITERAL_PATHSPECS` defaults, prefix-relative specs from a subdirectory and the implicit include
of the current directory for exclude-only lists follow Git.

`tests/pathspec.rs` builds an index through `git update-index --cacheinfo` (so case-only differences
survive case-insensitive filesystems) and compares girt's selection with `git ls-files --full-name`
for each spec in its corpus, from the root and from a subdirectory, with and without each
environment default. Specs Git rejects must be rejected by girt. The same test checks that
`Pathspec::directory` never skips a selected path or takes an unselected one.

Under `icase`, Git lowers the path byte before comparing it with a single bracket member, so
`:(icase)a[B]` matches nothing while `a[A-Z]` and `[[:upper:]]` accept either case. The shared
wildmatch engine follows that rule, which also applies to case-insensitive config include
conditions.

`attr:` magic (`name`, `-name`, `!name`, `name=value`, with backslash escapes in values) is compared
the same way against a `.gitattributes` file, through `Pathspec::matches_with` and
`girt::attributes`. Git's errors for an empty list, a second `attr:`, an invalid name and a trailing
escape are matched.

`Repository::status_with` and `raw_status_with` take a `Pathspec`. `tests/status_pathspec.rs`
compares `git status --porcelain -- <pathspec>` with girt's report for 31 spec lists, in both
`-unormal` (collapsed untracked directories) and `-uall` modes, and from a subdirectory. Git lists
an untracked directory as `dir/` only when the pathspec matches `dir/` itself; otherwise it lists
the matching files inside, and girt does the same. With only exclude specs, `git status` starts from
the whole repository while `git ls-files` starts from the current directory;
`Context::with_excludes_from_root` selects between them.

`Objects::compare_trees_matching` and `diff_trees_matching` limit tree comparison.
`tests/diff_pathspec.rs` compares them with `git diff-tree -r --name-status [-M] -- <pathspec>` for
21 spec lists over a history with modified, added, deleted, renamed and file-to-directory paths. As
in Git, the pathspec applies before rename detection, so a file moved into the pathspec from outside
it is an addition. The same test shows that a subtree outside the pathspec is never read: a missing
tree object there goes unreported.

`Objects::simplify_commit` applies Git's default history simplification for `git log -- <pathspec>`:
a commit is shown when it changes a selected path, and a merge that left the paths as one parent had
them is hidden and continues through that parent only. `girt log` uses it.
`girt-cli/tests/pathspec_parity.rs` compares `girt --git log` and `girt --git diff` with Git for 11
spec lists over a history with two merges (one made with `-s ours`, one followed by a resolving
commit), including `-n` limits and runs from a subdirectory. `--full-history`, `--simplify-merges`,
parent rewriting and `--follow` are not provided.

The current-directory prefix is matched literally and with case, as Git does: from a directory named
`a[b]`, `*.rs` selects `a[b]/x.rs`, and from `src`, `:(icase)*.rs` does not select `SRC/up.rs`.
`Pathspec::matches` returns `AttributesRequired` for an `attr:` spec rather than assume attribute
states; `Pathspec::matches_with` answers it.

Limitations: absolute paths are rejected even when they name a path inside the worktree, and status,
tree comparison and history refuse `attr:` magic.

## Saved stash objects

`stash::Snapshot` reads the base/index/optional-untracked parent shape produced by Git.
`Objects::create_stash` takes explicit captured trees and identities and constructs unsigned saved
commits without installing objects or changing refs/files. Default and custom worktree messages,
index/untracked messages, parent order and SHA-1/SHA-256 identities are compared with Git in
`tests/stash_git.rs`. Branch labels and unique abbreviation lengths are caller inputs.

All-changes and keep-index plans name the cleanup trees. Staged-only saves retain only the index
change, then compute inverse cleanup with the existing merge engine. Overlap retains structured
conflicts; Git's patch-based cleanup can instead refuse after publishing the saved entry. Callers
must retain the saved entry when cleanup cannot complete. Filesystem capture, filters and path
selection remain caller policy; no convenience command captures or deletes working files.

### Applying stashes and editing their history

`Objects::apply_stash` uses explicit-base tree merges and retains original conflict sides. Ordinary
apply preserves current staged entries and stages added paths; optional index restoration is a
separate merge that can stop before working-tree computation. Git-oracle cases compare index and
working trees, index/working conflicts and untracked collision refusals. Guarded apply/pop cases
publish working files and the intended index through the existing merge checkout under one guard,
then conditionally drop the saved entry. Filesystem capture and dirty save cleanup remain caller
policy, tracked in [the repository workflow follow-up](https://github.com/joshka/girt/issues/639).
Git's patch-based staged cleanup/index restoration can refuse inputs that tree merging resolves;
this API exposes tree-merge outcomes rather than promising patch admission parity.

`stash::History` retains bounded complete files-backend logs and lists entries newest first.
`tests/stash_history_git.rs` compares newest/middle/oldest drop bytes with Git, verifies surviving
objects remain readable, and covers last-entry deletion, explicit push, stale snapshots and foreign
locks. A fault-model test covers reference publication followed by log failure: the partial receipt
names the new tip and original log bytes survive. No object is deleted. Ref/log replacement is
sequential and requires caller isolation; it promises no cross-file crash transaction.
[Reftable stash history editing](https://github.com/joshka/girt/issues/638) is explicitly refused.

## In-memory text patch application

`tests/apply.rs` generates original text patches with Git 2.56.0 on macOS, under isolated
configuration and fixed identities/dates. SHA-1 and SHA-256 cases apply a Git-generated patch with
`git apply --cached`, then compare the resulting tree ID with a tree staged from girt's computed
postimage. Focused cases cover insertion, deletion, exact context, CR/non-UTF-8 bytes, missing final
LF, quoted paths, malformed/truncated counts, overflow, limits and late-hunk failure.

This establishes exact-position text application only. Combined diffs are explicitly refused.
Extended metadata and binary-patch support are described below. Offset/fuzz matching, whitespace
policy, reverse/recount, index/worktree publication and mail workflows remain
[issue 506](https://github.com/joshka/girt/issues/506). The `apply` fuzz target exercises bounded
parsing and application to independently framed preimages.

`tests/apply_tree.rs` compares in-memory tree application and explicit object installation with
`git apply --cached` tree IDs in SHA-1 and SHA-256 repositories. It also covers nested additions,
parent pruning, executable-mode preservation, mapped paths, collision refusals, late-hunk failure,
aggregate bounds, cancellation and cross-format installation refusal. Computation leaves generated
blobs and trees absent until installation. A separate test checks that opaque empty subtrees remain
present; Git's index cannot preserve these entries, so this case uses direct tree assertions.

The tree layer rejects `.git` components case-insensitively, as Git's cached application does. It
currently refuses replacing a directory emptied by earlier edits in the same patch; an oracle test
records Git accepting this transition. This remains part of issue 506. Metadata aliases that depend
on a filesystem are checked by checkout rather than interpreted by this tree operation.

`tests/apply_extended.rs` generates Git mode changes, empty additions/deletions, type changes,
renames, copies and binary patches from original SHA-1/SHA-256 trees. Both girt application and
`git apply --cached` produce the expected tree IDs. A hand-built rename cycle also checks Git and
establishes that sources come from the original snapshot. Binary cases exercise literal and delta
blocks, including incompressible literals, both block directions, identity failures, malformed
base85/zlib input and separate aggregate decode/output bounds. The decoder reuses girt's checked
pack-delta interpreter; no upstream implementation or test source is used.

A bounded `apply` libFuzzer campaign on macOS completed 90,869 executions in 21 seconds without
findings, seeded with original text and Git-generated binary patches framed with their preimages.
The command used `-max_total_time=20 -timeout=10 -rss_limit_mb=2048 -max_len=1048576`; peak reported
RSS was 288 MiB. This is short mutation evidence, not exhaustive parser validation.

### Configured index flags and file modes

`Repository::status` and `status_with` omit intent-to-add entries from staged content and report
existing working files as unstaged additions, including empty files. Missing intent-to-add files are
unstaged deletions. Assume-unchanged and skip-worktree suppress working differences while preserving
staged differences. `core.fileMode=false` ignores only executable-bit differences between regular
files; content and filesystem type changes still appear. These settings use the configuration loaded
into the repository handle. `Report::comparison` identifies configured versus raw results.

The original SHA-1 and SHA-256 fixtures in `tests/status.rs` compare these cases with the isolated
Git executable. Raw status retains its literal mode comparison, verifies assume-unchanged content
and refuses intent-to-add/skip-worktree. Neither status form authorizes overwriting working files;
checkout verifies its own baseline independently. Sparse-directory expansion and sparse-checkout
pattern selection remain separate operations. A promised file replaced by a directory keeps its
untracked children visible with `-uall`; the alias-directory comparison also checks Git's spelling
and pathspec selection on folding volumes.

### Configured symlink representation

Configured status, filtered checkout and `Repository::checkout_tree_configured` read `core.symlinks`
from the repository handle, defaulting to true. With false, a regular file's bytes are compared as
the target of an indexed symlink, and checkout writes that representation without changing the index
mode. Real symlinks still compare by their target bytes. Placeholder content changes remain unstaged
modifications; regular-file index entries retain ordinary type-change detection. Raw status and
checkout retain native-symlink semantics. Native symlinks bypass status conversion, and checkout of
symlink blobs bypasses smudge conversion even when writing placeholders. Filtered status reads
regular-file placeholders through clean/EOL conversion. An original isolated Git 2.56 fixture with
index mode `120000`, `core.symlinks=false` and `link filter=reject text eol=crlf` exits 128 when the
required clean driver is `exit 1`; an identity clean driver reports the same placeholder clean.
`tests/filter_status.rs::symlink_placeholders_apply_clean_filters` preserves that comparison, and
`tests/filter_checkout.rs::configured_symlink_placeholders_bypass_smudge_filters` verifies the
separate checkout rule in both object formats.

Original SHA-1/SHA-256 fixtures in `tests/status.rs` and `tests/checkout.rs` compare both settings
with isolated Git status and read-tree. Checkout independently verifies placeholders during
preparation, removal and final publication; assume-unchanged cannot hide a dirty target from these
checks. Configured checkout has the same platform, exclusion and recovery contract as raw checkout.
Its executable-bit baseline verification remains literal under `core.fileMode=false`;
[configured checkout file-mode policy](https://github.com/joshka/girt/issues/652) tracks that
conservative refusal separately.

## Signed payloads

`girt::signing` splits a commit or tag into its signature and the exact bytes the signature covers,
and signs and verifies OpenPGP, X.509 and SSH signatures.

`Commit::signed_payload` reads the signature header for the commit's own format (`gpgsig` for SHA-1,
`gpgsig-sha256` for SHA-256), unfolds it, and joins repeated headers. The payload drops every
`gpgsig` and `gpgsig-sha256` header. A commit with only the other format's header is unsigned.

`Tag::signed_payload` starts the signature at the last line, anywhere in the tag, that begins with
`-----BEGIN PGP SIGNATURE-----`, `-----BEGIN PGP MESSAGE-----`, `-----BEGIN SIGNED MESSAGE-----` or
`-----BEGIN SSH SIGNATURE-----`; text after the marker on that line is allowed. The payload removes
`gpgsig` and `gpgsig-sha256` headers in both hash formats, but Git removes only the last header of
each run of adjacent signature headers, and girt does the same. Signature-like lines in the message
are kept. `Commit::merge_tags` parses `mergetag` headers into tags whose IDs match the merged tag
objects.

`tests/signing.rs` replaces Git's `gpg.program`, `gpg.ssh.program` and `gpg.x509.program` with a
recorder and compares the payload and signature Git hands them during `verify-commit` and
`verify-tag` with girt's split, for SHA-1 and SHA-256 and for repeated, mixed, folded and misplaced
signatures. A second test signs commits, a tag and a merge of that tag through Git with a throwaway
ed25519 key and checks each split with `ssh-keygen -Y verify`. The recorder test needs a POSIX
shell; the ssh test skips when `ssh-keygen` is absent.

### SSH signing and verification

`SshSigner` runs `ssh-keygen -Y sign -n git -f <key>` with the payload on standard input, adding
`-U` and a temporary key file for a literal `key::` signing key, as Git does. `SshVerifier` follows
the sequence Git runs: `ssh-keygen -Y find-principals` against `gpg.ssh.allowedSignersFile`, then
`-Y verify` for the first principal printed, with `gpg.ssh.revocationFile` as `-r`; with no
principal, `-Y check-novalidate` without the revocation file. Both pass the committer or tagger date
as `-Overify-time`. Git formats that time in the local time zone; girt passes UTC with a `Z` suffix,
which `ssh-keygen` reads as the same instant. Nothing runs under `Programs::Refuse`, the default.

`tests/signing.rs` signs commits and tags through Git with throwaway ed25519 keys. Because ed25519
signatures are deterministic, it requires girt's signature and re-signed tag to equal Git's byte for
byte, in SHA-1 and SHA-256 repositories. It compares `Status::letter`, signer, fingerprint and trust
with `git log --format=%G?|%GS|%GF|%GT` for a good signature, an unknown key, `valid-before` and
`valid-after` outside and around the commit date, a disallowed namespace, several principals, a
revoked key with and without an owning principal, a missing allowed signers file, and a tampered
commit, and compares tags with `git verify-tag`'s exit status. Without `gpg.ssh.allowedSignersFile`,
girt returns `Error::NoAllowedSigners` where Git reports `N`. The `gpg.ssh.defaultKeyCommand` and
literal-key paths are not compared with Git, because they need an SSH agent.

### OpenPGP and X.509 signing and verification

`GpgSigner` runs `gpg --status-fd=2 -bsau <key>` (`gpgsm` for X.509) with the payload on standard
input and requires `SIG_CREATED`, as Git does; without `user.signingKey` it signs as the committer
`Name <email>`. The OpenPGP program is the later of `gpg.program` and `gpg.openpgp.program`.
`GpgVerifier` runs `gpg --keyid-format=long --status-fd=1 --verify <signature> -` (`gpgsm` without
`--keyid-format`) and reads `GOODSIG`, `BADSIG`, `EXPSIG`, `EXPKEYSIG`, `REVKEYSIG`, `ERRSIG`,
`VALIDSIG` and `TRUST_*` status lines. A good signature is `G` at `marginal` trust or better and `U`
below. `verify-commit` accepts `G`, `U` and `Y` at or above `gpg.minTrustLevel`, whose default
`undefined` sits above `never`; the same minimum applies to SSH, whose owned keys have `fully`
trust. `gpg.format` values are matched case-sensitively and `gpg.minTrustLevel` case-insensitively,
as Git does.

`tests/signing_gpg.rs` generates throwaway keys in temporary `GNUPGHOME` directories and stops their
agents afterwards. It checks that Git accepts commits girt signs and that girt reads Git's
signatures in SHA-1 and SHA-256 repositories. It compares `%G?|%GS|%GK|%GF|%GP|%GT` and
`verify-commit`'s exit status for unknown, `never`, `marginal` and `full` ownertrust under several
`gpg.minTrustLevel` values, a missing key, a tampered commit, an expired key, an expired signature,
a revoked key, a signed tag, and an X.509 certificate from `gpgsm`. Expired keys and signatures are
made with `--faked-system-time` through a POSIX shell wrapper; those cases and all tests skip when
`gpg` or `gpgsm` is absent.

### Index-only patch preparation

`tests/apply_index.rs` computes without a working tree, checks that no postimage object or index
write occurs before explicit installation/publication, and verifies Git reads the published tree. It
preserves unrelated flags/stat data and checks late hunk failure, cancellation and resource limits.
Unsupported conflict stages, intent-to-add and sparse entries return explicit errors.
`tests/apply_extended.rs` also computes index drafts alongside the existing isolated Git cached
application oracles for SHA-1/SHA-256 metadata, text, binary and reverse patches.

### Reverse patch application

`tests/apply_extended.rs` compares `Patch::reversed` tree IDs with isolated `git apply --cached -R`
for SHA-1/SHA-256 binary literals/deltas, additions/deletions, empty files and mode/type changes. A
text rename preserves missing final newlines, and reversing twice restores forward application.
Reverse copies exchange paths and retain copy behavior; both implementations refuse an occupied
reversed destination, and Git leaves its index unchanged. Missing reverse binary blocks are refused.
The existing directory-to-file limitation also applies to reversed patches.
