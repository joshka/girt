# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4](https://github.com/joshka/girt/compare/girt-cli-v0.2.3...girt-cli-v0.2.4) - 2026-10-17

### Other

- Align tree diff names with default-limit APIs ([#430](https://github.com/joshka/girt/pull/430))
- Add reference helpers and repository-format object IDs ([#429](https://github.com/joshka/girt/pull/429))
- Add snapshot and commit builders ([#424](https://github.com/joshka/girt/pull/424))
- Take defaults with short names; move explicit limits to _with ([#421](https://github.com/joshka/girt/pull/421))
- Add a crate-level error and Git display helpers ([#423](https://github.com/joshka/girt/pull/423))

## [0.2.3](https://github.com/joshka/girt/compare/girt-cli-v0.2.2...girt-cli-v0.2.3) - 2026-10-17

### Other

- Add current and configured signature constructors ([#410](https://github.com/joshka/girt/pull/410))

## [0.2.2](https://github.com/joshka/girt/compare/girt-cli-v0.2.1...girt-cli-v0.2.2) - 2026-10-17

### Other

- Load Git's default configuration sources in one library call ([#395](https://github.com/joshka/girt/pull/395))

## [0.2.1](https://github.com/joshka/girt/compare/girt-cli-v0.2.0...girt-cli-v0.2.1) - 2026-10-17

### Other

- Fix the main build: report index errors from IndexedPack::contains ([#388](https://github.com/joshka/girt/pull/388))
- List packs and enumerate objects through Objects; add girt object list and count ([#368](https://github.com/joshka/girt/pull/368))
- Add shell completions and man pages to the girt CLI ([#374](https://github.com/joshka/girt/pull/374))

## [0.2.0](https://github.com/joshka/girt/compare/girt-cli-v0.1.0...girt-cli-v0.2.0) - 2026-10-17

### Other

- Fix the girt-cli build after the SSH argument type change ([#381](https://github.com/joshka/girt/pull/381))
- Honor CLI color settings and style human output ([#379](https://github.com/joshka/girt/pull/379))
- Support SSH remotes in the CLI ([#361](https://github.com/joshka/girt/pull/361))
- Parse and resolve Git revisions and ranges ([#369](https://github.com/joshka/girt/pull/369))
- [**breaking**] name options setters with the with_ prefix ([#349](https://github.com/joshka/girt/pull/349))
- Show live CLI operation progress ([#373](https://github.com/joshka/girt/pull/373))
- Read the girt CLI log filter from GIRT_LOG instead of RUST_LOG ([#375](https://github.com/joshka/girt/pull/375))
- Support abbreviated object IDs in the library and CLI ([#359](https://github.com/joshka/girt/pull/359))
- Check out HEAD after girt clone and fix checkout without an index ([#367](https://github.com/joshka/girt/pull/367))
- Simplify the first-read docs with RefName::HEAD, Repository::head and Objects::read_commit ([#356](https://github.com/joshka/girt/pull/356))
- Support disposable index cache extensions ([#358](https://github.com/joshka/girt/pull/358))
