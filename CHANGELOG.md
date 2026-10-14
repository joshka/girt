# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.1](https://github.com/joshka/girt/compare/v0.9.0...v0.9.1) - 2026-10-14

### Other

- Share validated pack indexes between live readers ([#190](https://github.com/joshka/girt/pull/190))

## [0.9.0](https://github.com/joshka/girt/compare/v0.8.1...v0.9.0) - 2026-10-14

### Other

- Reduce loose reference enumeration overhead ([#192](https://github.com/joshka/girt/pull/192))
- Reduce verified tag peeling overhead ([#191](https://github.com/joshka/girt/pull/191))

## [0.8.1](https://github.com/joshka/girt/compare/v0.8.0...v0.8.1) - 2026-10-14

### Other

- Add value traits and name byte conversions ([#182](https://github.com/joshka/girt/pull/182))
- Test malformed pkt-line read boundaries ([#179](https://github.com/joshka/girt/pull/179))
- Test the remaining commit parent bound ([#180](https://github.com/joshka/girt/pull/180))
- Update rustls for RUSTSEC advisories ([#189](https://github.com/joshka/girt/pull/189))
- Serialize duplicate CI runs by PR head SHA ([#173](https://github.com/joshka/girt/pull/173))
- Follow protocol.version in Repository fetch workflows ([#169](https://github.com/joshka/girt/pull/169))
- Request protocol v2 over HTTP and SSH ([#165](https://github.com/joshka/girt/pull/165))

## [0.8.0](https://github.com/joshka/girt/compare/v0.7.2...v0.8.0) - 2026-10-14

### Changed

- **Breaking:** mark evolving public enums and structs `#[non_exhaustive]`. External enum matches
  need fallback arms; options and limits use `Default` with `with_*` methods, and inputs with required
  data use constructors instead of struct literals. Preserve exhaustive Git object kinds and index
  conflict stages, exact format records and existing opaque construction APIs. Future variant and
  field additions to extensible types can ship in compatible 0.8.x releases.
- Implement redacted `Debug` for remote configuration editors, native push context and terminal
  event/stop handles.

### Other

- Pack references after fetch, as Git's automatic maintenance does ([#164](https://github.com/joshka/girt/pull/164))
- Parse protocol v2 capabilities and delim/response-end pkt-lines ([#160](https://github.com/joshka/girt/pull/160))
- Request selected references with v2 ls-refs ref-prefix ([#161](https://github.com/joshka/girt/pull/161))
- Add bounded parser fuzzing for objects and packs ([#166](https://github.com/joshka/girt/pull/166))
- Fuzz pkt-line and fetch and push protocol parsers ([#167](https://github.com/joshka/girt/pull/167))
- Fuzz configuration, ignore, index and reference parsers ([#168](https://github.com/joshka/girt/pull/168))

## [0.7.2](https://github.com/joshka/girt/compare/v0.7.1...v0.7.2) - 2026-10-14

### Other

- Retry damaged pack entries from verified copies ([#155](https://github.com/joshka/girt/pull/155))
- Test history, content diff and status on SHA-256 ([#153](https://github.com/joshka/girt/pull/153))
- Create SHA-256 clones for SHA-256 sources ([#152](https://github.com/joshka/girt/pull/152))
- Test remote mapping on SHA-256 ([#154](https://github.com/joshka/girt/pull/154))

## [0.7.1](https://github.com/joshka/girt/compare/v0.7.0...v0.7.1) - 2026-10-14

### Other

- cache Rust builds across workflow runs ([#150](https://github.com/joshka/girt/pull/150))
- list references once per fetch ([#149](https://github.com/joshka/girt/pull/149))

## [0.7.0](https://github.com/joshka/girt/compare/v0.6.4...v0.7.0) - 2026-10-14

### Other

- hash and parse received objects on worker threads ([#133](https://github.com/joshka/girt/pull/133))
- cut redundant source reads in local fetch ([#132](https://github.com/joshka/girt/pull/132))
- verify received objects once ([#131](https://github.com/joshka/girt/pull/131))
- hash object ids with a seeded multiply of their digest ([#130](https://github.com/joshka/girt/pull/130))

## [0.6.4](https://github.com/joshka/girt/compare/v0.6.3...v0.6.4) - 2026-10-13

### Other

- Add maintenance, pack, reftable, locks, diff and network transport examples ([#146](https://github.com/joshka/girt/pull/146))
- Add working-tree and diff examples ([#145](https://github.com/joshka/girt/pull/145))
- Add fetch, clone, push and transport module examples ([#144](https://github.com/joshka/girt/pull/144))
- Add refs, remote and config module examples ([#143](https://github.com/joshka/girt/pull/143))
- Enable missing_docs and add core repository examples ([#142](https://github.com/joshka/girt/pull/142))

## [0.6.3](https://github.com/joshka/girt/compare/v0.6.2...v0.6.3) - 2026-10-13

### Other

- Report native local fetch and push validation progress ([#140](https://github.com/joshka/girt/pull/140))

## [0.6.2](https://github.com/joshka/girt/compare/v0.6.1...v0.6.2) - 2026-10-13

### Other

- Expose caller-controlled lock cleanup ([#137](https://github.com/joshka/girt/pull/137))

## [0.6.1](https://github.com/joshka/girt/compare/v0.6.0...v0.6.1) - 2026-10-13

### Other

- remove deep push connectivity cliff ([#128](https://github.com/joshka/girt/pull/128))

## [0.6.0](https://github.com/joshka/girt/compare/v0.5.0...v0.6.0) - 2026-10-13

### Other

- Quarantine and cancel local receive hooks ([#101](https://github.com/joshka/girt/pull/101))
- Batch pack header reads in retention operations ([#99](https://github.com/joshka/girt/pull/99))

## [0.5.0](https://github.com/joshka/girt/compare/v0.4.3...v0.5.0) - 2026-10-13

### Other

- Pack references with Git-identical packed-refs, and pack on clone ([#124](https://github.com/joshka/girt/pull/124))
- Read configured reflog expiry policies ([#95](https://github.com/joshka/girt/pull/95))
- Add jj everyday-operation benchmark against stock jj and Git ([#120](https://github.com/joshka/girt/pull/120))

## [0.4.3](https://github.com/joshka/girt/compare/v0.4.2...v0.4.3) - 2026-10-13

### Other

- bound local push connectivity checks ([#123](https://github.com/joshka/girt/pull/123))
- Fix a config glob panic and remove panics from config and ignore parsers ([#121](https://github.com/joshka/girt/pull/121))
- Remove panics from reference and reflog parsers ([#115](https://github.com/joshka/girt/pull/115))
- Remove panics from index file parsers ([#114](https://github.com/joshka/girt/pull/114))
- Remove panics from pack data and index parsers ([#113](https://github.com/joshka/girt/pull/113))
- Remove panics from object and pkt-line parsers ([#112](https://github.com/joshka/girt/pull/112))

## [0.4.2](https://github.com/joshka/girt/compare/v0.4.1...v0.4.2) - 2026-10-13

### Other

- Add a quality scorecard and fix stale release references ([#111](https://github.com/joshka/girt/pull/111))
- Share decoded pack payloads with the base cache ([#102](https://github.com/joshka/girt/pull/102))
- Skip retention walks for verified GC no-ops ([#96](https://github.com/joshka/girt/pull/96))
- Store an unstored empty tree that a native push depends on ([#98](https://github.com/joshka/girt/pull/98))

## [0.4.1](https://github.com/joshka/girt/compare/v0.4.0...v0.4.1) - 2026-10-13

### Other

- Rebuild a damaged pack index from its pack ([#89](https://github.com/joshka/girt/pull/89))

## [0.4.0](https://github.com/joshka/girt/compare/v0.3.4...v0.4.0) - 2026-10-13

### Other

- Open pack indexes with linear structural checks ([#82](https://github.com/joshka/girt/pull/82))
- Stop verified fast-forward walks at the old tip ([#85](https://github.com/joshka/girt/pull/85))
- Reuse verified receive evidence in fetch and clone ([#83](https://github.com/joshka/girt/pull/83))
- Fix Windows HTTP post-send failure assertion ([#79](https://github.com/joshka/girt/pull/79))
- Generate push deltas against advertised history ([#78](https://github.com/joshka/girt/pull/78))
- Cut per-entry read cost in walks, retirement and repack ([#77](https://github.com/joshka/girt/pull/77))
- Answer credential.helper = store natively ([#70](https://github.com/joshka/girt/pull/70))
- Share retention plans and locate blob edges ([#71](https://github.com/joshka/girt/pull/71))

## [0.3.4](https://github.com/joshka/girt/compare/v0.3.3...v0.3.4) - 2026-10-12

### Other

- Reuse compressed entries in native transfers ([#72](https://github.com/joshka/girt/pull/72))
- Prepare pushes against the receiver's advertisement ([#69](https://github.com/joshka/girt/pull/69))
- Defer whole-pack validation until requested ([#67](https://github.com/joshka/girt/pull/67))

## [0.3.3](https://github.com/joshka/girt/compare/v0.3.2...v0.3.3) - 2026-10-12

### Other

- Add storage-safety and performance-measurement rules ([#48](https://github.com/joshka/girt/pull/48))
- Require a specification before mutating-storage and performance work ([#59](https://github.com/joshka/girt/pull/59))
- Repack by copying existing pack entries and their deltas ([#60](https://github.com/joshka/girt/pull/60))
- Stop SSH test fixtures leaking and stalling under load ([#50](https://github.com/joshka/girt/pull/50))
- Disable MD013 for generated CHANGELOG.md ([#56](https://github.com/joshka/girt/pull/56))
- Wrap over-long changelog line ([#54](https://github.com/joshka/girt/pull/54))

### Changed

- Repack reuses existing packed entries and deltas. `RepackLimits::default()` and `trusted()` now enable
  bounded delta compression for newly packed objects instead of writing only whole objects. Copied entries
  count against logical input and object byte limits. Required and recent unreachable objects keep
  a verified readable copy and dependency graph; old unreachable objects may be reclaimed.
- Additive repack includes protected objects and remains byte-stable on repeated runs. Retirement
  avoids duplicate readable protected copies. `PackWritten::deltas` includes copied chains.

## [0.3.2](https://github.com/joshka/girt/compare/v0.3.1...v0.3.2) - 2026-10-11

### Other

- Skip Rust CI for docs-only changes ([#40](https://github.com/joshka/girt/pull/40))
- Gate merges on a single required CI check ([#36](https://github.com/joshka/girt/pull/36))
- Prune roadmap-phase process from agent guidance ([#38](https://github.com/joshka/girt/pull/38))
- Isolate reftable fixtures from global config portably
  ([#35](https://github.com/joshka/girt/pull/35))
- Stream received packs with bounded delta caching
- Track jj follow-up work as GitHub issues
- Add large-repository and no-Git checks for jj
- Widen lock-wait test margins for Windows runners
- Retry Windows ref locks that are still being released
- Escape Windows paths written into test configuration

## [0.3.1](https://github.com/joshka/girt/compare/v0.3.0...v0.3.1) - 2026-09-30

### Other

- Record the large-repository benchmark
- Drop an import unused on Windows
- Cache delta bases and check only requested objects
- Look up packed objects before loose ones
- Verify only new history when publishing a fetch
- Use trusted limits for configured-remote transfers
- Name CI workflows and jobs for what they check
- Record jj on published girt
- Gate platform-specific code for Windows builds
- Release from main

## [0.3.0](https://github.com/joshka/girt/compare/v0.2.0...v0.3.0) - 2026-09-30

### Other

- Support jj as the sole Git backend ([#15](https://github.com/joshka/girt/pull/15))

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
