# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Repository::{fetch, push, remote_head}` run configured-remote transfers over local, HTTP(S) and
  SSH transports, with credential helpers and `GIT_SSH_COMMAND`
- `InitOptions` creates repositories like `git init`; `InitOptions::defaults_from` applies
  `init.defaultBranch`
- `Repository::logs_updates_to` decides reflog creation from `core.logAllRefUpdates`
- `References::list_namespace_observations` lists a namespace with packed peeled hints
- `trusted()` limits for reading, packs, peeling, the index and ignore rules in repositories the
  caller trusts
- `FetchError::FormatMismatch` for a remote with a different object format

### Changed

- Push preparation walks only history the receiver lacks, instead of every reachable object
- Native local push runs receive hooks instead of refusing the push
- Index writes smudge racily clean entries instead of writing an epoch modification time
- Configuration edits write unquoted values where Git would, and remove whole lines

### Fixed

- Stop repeating an error's source in its `Display` message, so rendered error chains show each
  cause once

## [0.2.0](https://github.com/joshka/girt/compare/v0.1.5...v0.2.0) - 2026-09-29

### Breaking

- `Reflog` adds `AppendIfChanged` and `AppendExistingIfChanged`. Code that matches every `Reflog`
  variant must handle these new cases. The first appends only when the stored reference target
  changes; the second also requires an existing log.

### Other

- Use OpenSSL for Windows HTTPS fixture oracle ([#14](https://github.com/joshka/girt/pull/14))
- Define conservative native worktree cleanup
- Document Git reftable timezone mismatch
- Report native fetch validation progress
- Preserve disabled reflog creation policy
- Append reflogs only for changed targets
- Disable revocation lookup for fixture CA
- Make HTTPS oracle trust explicit on Windows
- Retain initial shallow fetch publication
- Stream HTTP fetch sideband notices
- Stream HTTP push sideband notices
- Record local transport integration status
- Define explicit HTTPS trust configuration
- Preserve continued configuration whitespace
- Fix SSH command configuration precedence

## [0.1.5](https://github.com/joshka/girt/compare/v0.1.4...v0.1.5) - 2026-09-27

### Other

- Stabilize split-index overlap fixture ([#12](https://github.com/joshka/girt/pull/12))
- Retain complete fetch packs through publication ([#9](https://github.com/joshka/girt/pull/9))
- Fix tracing capture after unsubscribed setup ([#10](https://github.com/joshka/girt/pull/10))

## [0.1.4](https://github.com/joshka/girt/compare/v0.1.3...v0.1.4) - 2026-09-27

### Other

- Expose explicit TREE cache invalidation ([#7](https://github.com/joshka/girt/pull/7))

## [0.1.3](https://github.com/joshka/girt/compare/v0.1.2...v0.1.3) - 2026-09-27

### Other

- Parse Git remote URLs for consumers ([#6](https://github.com/joshka/girt/pull/6))
- Teach the public API reading path
- Clarify documentation entry points

## [0.1.2](https://github.com/joshka/girt/compare/v0.1.1...v0.1.2) - 2026-09-27

### Other

- Expose effective remote URLs for display

## [0.1.1](https://github.com/joshka/girt/compare/v0.1.0...v0.1.1) - 2026-09-27

### Other

- Expose HTTP push advertisement before send
- Record initial release completion
- Adopt release-plz automation
