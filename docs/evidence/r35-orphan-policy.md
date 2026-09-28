# R35: Explicit Orphan-Creation Policy

`OrphanWorktreeOptions` adds caller-captured index path/limits, transformed private configuration,
shared permissions, file synchronization and link spelling to orphan creation. Existing wrappers
retain default policies. This producer API does not capture effective configuration or environment,
remove Git worktree add, or complete A19. The accepted prerequisites below support the remaining
consumer source-context adapter.

## Observed Scope

Original installed-Git probes cover 86 shared-policy executions and 78 Trace2 synchronization
executions across both object formats and reference backends. Shared permissions affect the
worktrees root, reference directories, HEAD, selected index and private reftable files.
Registration, link and private-configuration files retain ordinary umask. Group/everybody
adjustments retain existing world bits; exact modes require owner read/write, discard file execute
bits and derive directory traversal from read bits. The library never changes process-global umask.

The caller parses effective policy into `SharedPermissions`; this API does not duplicate Git's
configuration parser. Invalid exact modes fail before creation, without claiming identical Git
partial-effect timing. Non-Unix hosts currently support only the umask policy; this boundary cannot
justify whole-endpoint removal on unsupported platforms.

Index synchronization applies to both reference backends. Reference synchronization applies to the
new private reftable files and placeholders; files-backend unborn creation performs no reference
flush. The native operation does not reproduce Git's subprocess flush counts. macOS uses full file
flush; other platforms use `File::sync_all`. Failures are propagated, not ignored. No
directory-entry, crash or power-loss durability is claimed, and Windows durability has not been
qualified.

## Publication and Failure

Selected indexes use `IndexEdit` for locking, parsing, standalone draft replacement, permission
application and optional synchronization before rename. Original primary/shared snapshots and lock
checks remain active. Permission or sync failure does not publish the index draft. Private
configuration bytes are copied from the caller's snapshot; there is no new configuration-sync flag
or hook runner.

Reference-file sync failures identify the created file and retained registration through
`CreateWorktreeError::Synchronize`. Other partial creation errors retain their paths and
registration. There is no rollback or Git retry after failure. These guarantees require the existing
exclusion of competing administrators and path replacement.

## Producer Validation

Both-format/backend creation tests pass 28 new policy cases and 12 existing cases. Nine observed
mode projections and two reference-sync failure cases pass, as do 131 index-storage tests including
injected prepublication sync failure and invalid-mode cleanup. Tests preserve redirected index
storage, captured private configuration and ordinary modes on unselected files. Native platform and
source-context qualification remain separate gates.

## Accepted Prerequisites

The accepted producer revisions are policy `40d3425d`, command environment `2613ec55`, metadata
creation `118ba6e1` and command layout `36db43ea`. `Config::from_command_environment` parses counted
pairs followed by legacy command parameters; explicit command overrides follow through
`ConfigInputs`. It does not read ambient environment or add author/committer settings.

`RepositoryMetadata::create_orphan_worktree_with_options` returns a `RepositoryLocation` without
opening unrelated object or shallow state. `RepositoryLocation::at_git_dir_with_storage` selects
explicit common/object paths, and `read_metadata_for_command` uses captured working directory and
worktree selection. Paths must be absolute; these APIs neither capture environment nor establish
trust. Command setup requires a direct repository-format version and does not support configured
worktree interpolation. Ordinary repository opening retains its validation contract.

Metadata validation passed 2,831 unit, 341 integration and 50 documentation tests. Layout validation
passed 77 unit, 114 integration and 22 core cases. Combined checks passed 22 layout and 61 policy
cases, including redirected common storage, directory-shaped `commondir`, external objects, missing
default object storage and malformed shallow metadata. Strict lint, format and documentation gates
passed for the accepted producers.

The accepted jj policy adapter is `b3a025f2` (11 focused tests); captured `WorktreeCommandInputs` is
`0040e381` (15 focused tests, with 30 combined cases and strict Clippy). Consumer `command_config`
integration `71b3c400` passes 32 distinct tests, strict library/CLI Clippy and full formatting with
the lock file unchanged; nine source-selection scenarios agree with installed Git. It admits an
explicit system source or disablement, preserves captured physical sources and includes, and applies
counted pairs, legacy parameters and actual CLI overrides in order. Fresh Unix ownership checks
cover private, common, object and effective worktree directories plus source leaves. Unqualified
installation defaults, empty HOME, trust and platform cases retain the older native/`gix` path.
Creation failures in both native paths retain the temporary checkout for recovery; configuration
promotion errors remain contextual through the subsequent HEAD update. The `gix` bootstrap and Git
worktree-add endpoint remain: four production Git endpoints and five callers, with none removed by
this increment.

## Index Failure Timing

Original Git probes with malformed or directory-shaped `GIT_INDEX_FILE` show that worktree add
promotes common configuration before failing, even though no registration or destination remains.
The producer refuses the selected index before registration; the consumer preserves the promoted
configuration. This error does not imply unchanged common configuration or failure before all
publication. The fixture records are `more/results.json` (`index_existing`) and
`index-directory-results.txt` in the original temporary orphan-policy probe collection.
