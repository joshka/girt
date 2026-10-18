# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.10](https://github.com/joshka/girt/compare/girt-cli-v0.2.9...girt-cli-v0.2.10) - 2026-10-18

### Other

- Reset with --keep and --merge, and remove emptied directories on hard reset ([#709](https://github.com/joshka/girt/pull/709))
- Reset working files as git reset --hard does ([#693](https://github.com/joshka/girt/pull/693))
- Publish bitmap indexes during explicit maintenance ([#729](https://github.com/joshka/girt/pull/729))
- Fix reset git hints and an ambiguous trailers doc link ([#730](https://github.com/joshka/girt/pull/730))
- Paint ahead/behind and range walks over commit-graph positions ([#720](https://github.com/joshka/girt/pull/720))
- List configured HTTP remotes with credentials ([#723](https://github.com/joshka/girt/pull/723))
- Mark local changes in girt history describe with --dirty and --broken ([#702](https://github.com/joshka/girt/pull/702))
- Name commits by their nearest tag as git describe does ([#676](https://github.com/joshka/girt/pull/676))
- Write and verify multi-pack indexes ([#686](https://github.com/joshka/girt/pull/686))
- Reset the branch and index paths as git reset --soft, --mixed and -- <paths> do ([#681](https://github.com/joshka/girt/pull/681))
- Count commits ahead and behind as git rev-list --left-right --count does ([#656](https://github.com/joshka/girt/pull/656))

## [0.2.9](https://github.com/joshka/girt/compare/girt-cli-v0.2.8...girt-cli-v0.2.9) - 2026-10-18

### Other

- Check out clones through filters ([#618](https://github.com/joshka/girt/pull/618))
- Apply filters in Repo and the CLI by default ([#598](https://github.com/joshka/girt/pull/598))
- Limit log to a pathspec with Git's default simplification ([#588](https://github.com/joshka/girt/pull/588))
- Limit tree diff to a pathspec ([#586](https://github.com/joshka/girt/pull/586))
- Limit status to a pathspec ([#585](https://github.com/joshka/girt/pull/585))

## [0.2.8](https://github.com/joshka/girt/compare/girt-cli-v0.2.7...girt-cli-v0.2.8) - 2026-10-18

### Other

- Point Git users at girt equivalents for Git commands and flags ([#490](https://github.com/joshka/girt/pull/490))
- Center CLI docs on using girt ([#480](https://github.com/joshka/girt/pull/480))
- Give CLI commands their own docs section ([#473](https://github.com/joshka/girt/pull/473))

## [0.2.7](https://github.com/joshka/girt/compare/girt-cli-v0.2.6...girt-cli-v0.2.7) - 2026-10-18

### Other

- Apply diff attributes to patches and blame ([#463](https://github.com/joshka/girt/pull/463))

## [0.2.6](https://github.com/joshka/girt/compare/girt-cli-v0.2.5...girt-cli-v0.2.6) - 2026-10-18

### Other

- updated the following local packages: girt

## [0.2.5](https://github.com/joshka/girt/compare/girt-cli-v0.2.4...girt-cli-v0.2.5) - 2026-10-17

### Other

- Rename the convenience handle to Repo ([#446](https://github.com/joshka/girt/pull/446))
- Add recorded identity date formatting ([#445](https://github.com/joshka/girt/pull/445))
- Add bounded line attribution with rename following ([#444](https://github.com/joshka/girt/pull/444))
- Add unified patch output for tree and content diffs ([#443](https://github.com/joshka/girt/pull/443))
- Add configured clone with checkout ([#438](https://github.com/joshka/girt/pull/438))
- Add a trusted repository convenience handle ([#432](https://github.com/joshka/girt/pull/432))
- Add ignore-aware status and repository ignore rules ([#434](https://github.com/joshka/girt/pull/434))

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
