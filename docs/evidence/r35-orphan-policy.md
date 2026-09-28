# R35: Explicit Orphan-Creation Policy

`OrphanWorktreeOptions` adds caller-captured index path/limits, transformed private configuration,
shared permissions, file synchronization and link spelling to orphan creation. Existing wrappers
retain default policies. This producer API does not capture effective configuration or environment,
remove Git worktree add, or complete A19. Those require a separately qualified source-context
adapter.

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
source-context qualification remain separate gates. A subsequent paired probe confirms that Git
orphan creation accepts malformed or directory-shaped shallow metadata while full repository opening
rejects it. Metadata-backed creation is a separate prerequisite; this policy API does not bypass
that validation or substitute an empty shallow snapshot.
