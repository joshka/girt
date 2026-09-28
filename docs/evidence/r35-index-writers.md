# R35: Native Index Writers

The accepted jj change `qlwzmlkr` at `43f0b595`, above `b580282e`, removes both production `gix`
index writers. The previous writer helpers remain only under test configuration. Repository
bootstrap still uses `gix`; this acceptance does not establish complete A19 replacement. The
production Git executable inventory remains four endpoints and five callers.

## Producer Contract

`Repository::edit_index_at_with_options` adds explicit `EditOptions` for alternate split indexes and
following a final symlink. Existing APIs retain their defaults. Split dependencies resolve beside
the selected lexical index path, never beside a symlink referent. Missing shared storage fails
without publication; it is not interpreted as an empty index.

`IndexEdit::replace_index` validates a complete standalone draft against the held format and limits
without replacing its primary, shared-file or lock guards. Supplied `link` extensions are rejected
rather than introducing a dependency outside the snapshot. `discard_optional_extensions` atomically
drops explicitly selected optional data, invalidates entry-offset caches when needed and normalizes
split drafts while retaining the shared-file publication check. Mandatory structural extensions
cannot be dropped through this API; standalone sparse markers remain intact.

Symlink publication replaces the selected leaf and never writes its referent. Checks retain the
original leaf kind and link spelling, followed bytes and presence, shared snapshot and lock
identity. Unix additionally checks leaf device/inode identity. Existing cooperating-writer and
durability limitations remain; this does not promise protection against every noncooperating
filesystem race.

## Acceptance

The producer passes 351 index unit tests, including 54 new focused cases, 116 integration tests and
five public documentation tests. Strict all-feature/all-target Clippy, core compilation, docs.rs and
formatting checks pass. Cases cover both object formats, atomic replacement failures, optional
drops, split dependencies, symlink publication and observed concurrent changes.

The consumer passes 96 distinct tests: 84 Git integration cases and 12 index-builder cases. Strict
library and CLI all-target Clippy with `test-fakes`, full formatting and independent review pass.
The canonical lock file remains unchanged (reported digest prefix `86bda001`). Missing shared-index
storage is a terminal, nondestructive error. Sparse expansion before additions prevents duplicate
entries. Missing shared storage does not reproduce the observed destructive compatibility behavior.

Actual dependency-feature pruning and native platform qualification are separate gates. Removing
these writers does not imply that repository bootstrap or all index-related dependency paths have
been removed.
