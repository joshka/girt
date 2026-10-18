# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.0](https://github.com/joshka/girt/compare/girt-v0.16.0...girt-v0.17.0) - 2026-10-18

### Behaviour changes

- Repack and gc in bare repositories write reachability bitmaps only when `repack.writeBitmaps` or
  `pack.writeBitmaps` is explicitly true. This is a temporary difference from Git while bitmap
  writing is made faster (#732).
- gpg/gpgsm output without a verdict maps to `NoSignature` (N), as Git does (#719).
- `reset --hard`, `--keep` and `--merge` are supported on macOS and Linux and return `Unsupported` on
  Windows (#735).

### Added

- Reset: `--hard`, `--keep`, `--merge` and path resets. Untracked directories are replaced only if
  they contain nothing but empty directories, a documented difference from Git.
- `describe` with exact distances, describe revisions, `--dirty`/`--broken`; message cleanup and
  trailers; ahead/behind counts; notes; mailmap (the CLI log maps identities by default).
- Signing and verification: SSH, OpenPGP and X.509.
- Apply: text, binary and extended-header patches, onto trees and the index.
- Submodules: parsing, sources, status, init/update plans and execution.
- Multi-pack index reading and writing; reachability bitmap reading, queries and opt-in writing.
- Transport: TLS client certificates, certificate checks, askpass, `http.extraHeader` and
  `http.userAgent`.
- `girt::signing` splits signed commits and tags into the signature and the exact payload it
  signs, matching what Git passes to `gpg`, `gpgsm` and `ssh-keygen` when verifying.
  `Commit::merge_tags` reads the tags a merge recorded in `mergetag` headers.
- `signing::SshSigner` and `signing::SshVerifier` sign and verify SSH signatures with
  `ssh-keygen`, configured from `user.signingKey` and `gpg.ssh.*`, and only when the caller passes
  `Programs::Run`. Verification returns a typed `Verification` matching Git's `%G?` letters.
  `Tag::new_signed` signs a tag as `git tag -s` does.

### Performance

- Commit-graph records are read in place on open, about 760 → 45 ms on Linux v6.1 (#733).
- Ahead/behind counts and range walks stop by generation: about 2 ms against Git's 26 ms on git.git,
  and 8.8 ms against 49 ms on Linux v6.0...v6.1 (#720).
- Consolidating gc verifies the replacement pack in parallel, reaching `git gc` parity on git.git
  (#667).

### Other

- Build reset on Windows ([#735](https://github.com/joshka/girt/pull/735))
- Assert unset repacks disable bitmap output ([#736](https://github.com/joshka/girt/pull/736))
- Read commit-graph records in place instead of decoding them on open ([#733](https://github.com/joshka/girt/pull/733))
- Require explicit repack bitmap configuration ([#732](https://github.com/joshka/girt/pull/732))
- Reset with --keep and --merge, and remove emptied directories on hard reset ([#709](https://github.com/joshka/girt/pull/709))
- Reset working files as git reset --hard does ([#693](https://github.com/joshka/girt/pull/693))
- Publish bitmap indexes during explicit maintenance ([#729](https://github.com/joshka/girt/pull/729))
- Write reachability bitmap artifacts ([#724](https://github.com/joshka/girt/pull/724))
- Fix reset git hints and an ambiguous trailers doc link ([#730](https://github.com/joshka/girt/pull/730))
- Add a load gate to the jj benchmark harness ([#678](https://github.com/joshka/girt/pull/678))
- Fix Windows submodule precondition warnings ([#728](https://github.com/joshka/girt/pull/728))
- Make signing tests pass on the CI runners' gpgsm and Windows ssh-keygen ([#719](https://github.com/joshka/girt/pull/719))
- Paint ahead/behind and range walks over commit-graph positions ([#720](https://github.com/joshka/girt/pull/720))
- List configured HTTP remotes with credentials ([#723](https://github.com/joshka/girt/pull/723))
- Apply submodule initialization and checkout plans ([#708](https://github.com/joshka/girt/pull/708))
- Mark local changes in girt history describe with --dirty and --broken ([#702](https://github.com/joshka/girt/pull/702))
- Query reachable object differences with bitmaps ([#713](https://github.com/joshka/girt/pull/713))
- Read reachability bitmaps and reverse indexes ([#710](https://github.com/joshka/girt/pull/710))
- Ask GIT_ASKPASS and core.askPass for HTTP credentials ([#716](https://github.com/joshka/girt/pull/716))
- Resolve git describe output as a revision ([#701](https://github.com/joshka/girt/pull/701))
- Add a TLS certificate check hook for HTTPS ([#712](https://github.com/joshka/girt/pull/712))
- Fix the Windows submodule rustdoc link ([#707](https://github.com/joshka/girt/pull/707))
- Fix Windows maintenance compilation ([#697](https://github.com/joshka/girt/pull/697))
- Support TLS client certificates for HTTP ([#704](https://github.com/joshka/girt/pull/704))
- Plan submodule initialization and updates ([#694](https://github.com/joshka/girt/pull/694))
- Name commits by their nearest tag as git describe does ([#676](https://github.com/joshka/girt/pull/676))
- Write and verify multi-pack indexes ([#686](https://github.com/joshka/girt/pull/686))
- Inspect submodule checkouts and nested status ([#683](https://github.com/joshka/girt/pull/683))
- Run gpg signing comparisons on Unix only ([#691](https://github.com/joshka/girt/pull/691))
- Reset the branch and index paths as git reset --soft, --mixed and -- <paths> do ([#681](https://github.com/joshka/girt/pull/681))
- Parallelise consolidation's plan walk and survivor decode ([#667](https://github.com/joshka/girt/pull/667))
- Skip the URL trailer comparison on Git older than 2.56 ([#685](https://github.com/joshka/girt/pull/685))
- Support http.extraHeader and http.userAgent ([#680](https://github.com/joshka/girt/pull/680))
- Gate Unix-only signing test cases on Windows ([#682](https://github.com/joshka/girt/pull/682))
- Read multi-pack indexes for object lookup ([#677](https://github.com/joshka/girt/pull/677))
- Prepare and publish index-only patch drafts ([#675](https://github.com/joshka/girt/pull/675))
- Reverse parsed text and binary patches ([#674](https://github.com/joshka/girt/pull/674))
- Read submodule snapshots and resolve relative URLs ([#672](https://github.com/joshka/girt/pull/672))
- Sign and verify OpenPGP and X.509 signatures with gpg and gpgsm ([#669](https://github.com/joshka/girt/pull/669))
- Correct index publication timestamp docs ([#673](https://github.com/joshka/girt/pull/673))
- Read and validate submodule definitions ([#668](https://github.com/joshka/girt/pull/668))
- Read and add commit message trailers as git interpret-trailers does ([#660](https://github.com/joshka/girt/pull/660))
- Clean up commit messages as Git does before committing ([#650](https://github.com/joshka/girt/pull/650))
- Count commits ahead and behind as git rev-list --left-right --count does ([#656](https://github.com/joshka/girt/pull/656))

## [0.16.0](https://github.com/joshka/girt/compare/girt-v0.15.0...girt-v0.16.0) - 2026-10-18

### Breaking changes

- `Repository::raw_status_with` and `Repository::status_with` take a `paths: &Pathspec` argument;
  pass `&Pathspec::all()` for the previous behaviour (#549–#592).
- Checkout no longer refuses colliding paths. On case-sensitive volumes both paths of a pair are
  written (for example linux.git's `xt_CONNMARK.h` and `xt_connmark.h`). Where the volume or
  `core.ignoreCase` folds names, one path per set is written as Git chooses and listed in the new
  `Report::collisions`. A file and a directory sharing a name, case-only renames (#620) and existing
  untracked aliases are still refused (#614–#629).
- Checkout refuses Git's `core.protectNTFS`/`core.protectHFS` `.git` aliases (`git~1`, `.git.`,
  `.git `, `.git:stream`, and HFS-ignorable spellings on macOS) unless those options are false.

### Behaviour changes

- Object readers on `Repository` load `.git/objects/info/commit-graph` (including split chains) by
  default, and history walks, merge-base and ancestry queries trust it after checksum and structural
  validation (#497–#590). Set `core.commitGraph=false` to disable it; strict per-object
  verification is opt-in.
- `Repo::status`, `Repo::switch`, `Repo::clone`, and `workflow::clone`/`clone_with` with default
  options convert files as Git does and run the filter programs that `.gitattributes` selects. Opt
  out with `Repo::with_filter_commands(Commands::Refuse)`,
  `CloneOptions::with_filter_commands(Some(Commands::Refuse))`, or `None` for a raw clone checkout.
  `girt status` and `girt checkout` convert by default; `--raw` restores the old behaviour. No
  compile-time breaks: the extended types are `#[non_exhaustive]` (#539–#618).
- Status and checkout accept `:`, `\` and non-ASCII names; `status`, `status_with` and
  `status_with_filters` apply `core.ignoreCase` (and `core.precomposeUnicode` on macOS).
- Normal and filtered status honour assume-unchanged, skip-worktree and intent-to-add entries,
  `core.fileMode` and `core.symlinks`. Raw APIs are unchanged (#636–#640).

### Added

- The merge API: three-way byte content merging with whitespace policies, rename-aware in-memory
  tree and commit merging with clean recursive bases, structured original conflict stages, explicit
  per-path merge drivers, and guarded marker and index publication (#522–#580). Known partial
  limits: #498, #543, #552, #575.
- In-memory cherry-pick, revert and rebase on the merge engine, a rebase todo-list parser, and
  interoperation with Git's sequencer state (#611, #623, #624). Known limits: #616, #621, #622.
- Stash: save, apply, pop and list on the merge engine (#632–#641). Known limits: #638, #639.
- `girt::pathspec`: Git pathspec parsing and matching, with path-limited status, diff and log.
- `girt::filter`: `text`/`eol`/`text=auto`, `core.autocrlf`, `core.eol`, `core.safecrlf`, `ident`,
  `working-tree-encoding` (UTF-16/32), and `filter.<name>.clean`/`smudge`/`process`, including the
  long-running process protocol used by Git LFS. Running filter programs is explicit, through
  `filter::Commands::Refuse` (the default for `Drivers`) or `Commands::Run`. Also
  `Repository::filters`, `filters_with_source` and `filters_for_checkout` (honouring `attr.tree` and
  `GIT_ATTR_SOURCE`), `Repository::status_with_filters`, `checkout_tree_with_filters`,
  `CloneOptions::filter_commands`, `Repo::with_filter_commands` and
  `filter::Pipeline::with_settings`.
- Commit-graph reading, writing (full and incremental) and changed-path Bloom filters.
- Unified text patch parsing and application in memory.
- `checkout::Collision`, `Report::collisions`, `status::Comparison::Configured`, `Change::Added`
  and `checkout_tree_configured[_with]`.
- CLI: `girt status` and `girt checkout` gain `--raw` and `--refuse-filter-commands`;
  `girt object hash --path`.
- New dependency: `unicode-normalization` 0.1 (MIT/Apache; macOS and Linux only).

### Performance

- Faster commit-graph attachment: range queries are about 1.8× Git and ancestry queries about 2.8×
  Git, down from about 7–11× (#625, #627).
- Loose objects are written at Git's default compression and published with one rename: about
  790 → 290 µs per object (#651).
- Faster `jj git clone` of a local path through the adapter (#606).

### Other

- Register apply errors in the crate wrapper ([#665](https://github.com/joshka/girt/pull/665))
- Initialize apply fuzz limits directly ([#658](https://github.com/joshka/girt/pull/658))
- Apply configured symlink representations ([#640](https://github.com/joshka/girt/pull/640))
- Apply index flags and file mode policy in status ([#636](https://github.com/joshka/girt/pull/636))
- Publish Linux jj file annotate on girt 0.14.0 (1.14x, below the 1.2x line) ([#637](https://github.com/joshka/girt/pull/637))
- Write loose objects at Git's default compression and publish them with one rename ([#651](https://github.com/joshka/girt/pull/651))
- Parse and apply unified text patches in memory ([#648](https://github.com/joshka/girt/pull/648))
- Apply core.ignoreCase and core.precomposeUnicode in status ([#629](https://github.com/joshka/girt/pull/629))
- Follow the volume's name rules in checkout: Unicode names, Git's collision handling ([#628](https://github.com/joshka/girt/pull/628))
- Accept colon and backslash working-tree names; refuse Git's NTFS/HFS .git aliases ([#614](https://github.com/joshka/girt/pull/614))
- Complete stash save compatibility evidence ([#646](https://github.com/joshka/girt/pull/646))
- Apply stashes and edit saved history explicitly ([#641](https://github.com/joshka/girt/pull/641))
- Compute and read Git stash snapshots ([#632](https://github.com/joshka/girt/pull/632))
- Normalize the fetch reflog fixture path ([#643](https://github.com/joshka/girt/pull/643))
- Check out clones through filters ([#618](https://github.com/joshka/girt/pull/618))
- Apply filters in Repo and the CLI by default ([#598](https://github.com/joshka/girt/pull/598))
- Convert working-tree-encoding for UTF-16 and UTF-32 ([#596](https://github.com/joshka/girt/pull/596))
- Write converted files in checkout ([#573](https://github.com/joshka/girt/pull/573))
- Compare converted working files in status ([#568](https://github.com/joshka/girt/pull/568))
- Speak the long-running filter process protocol (Git LFS) ([#561](https://github.com/joshka/girt/pull/561))
- Run filter clean and smudge commands (girt::filter) ([#548](https://github.com/joshka/girt/pull/548))
- Add line-ending and ident conversion (girt::filter) ([#539](https://github.com/joshka/girt/pull/539))
- Initialize replay test limits directly ([#634](https://github.com/joshka/girt/pull/634))
- Limit log to a pathspec with Git's default simplification ([#588](https://github.com/joshka/girt/pull/588))
- Limit tree diff to a pathspec ([#586](https://github.com/joshka/girt/pull/586))
- Limit status to a pathspec ([#585](https://github.com/joshka/girt/pull/585))
- Support attr: pathspec magic ([#558](https://github.com/joshka/girt/pull/558))
- Add girt::pathspec for parsing, matching and pruning pathspecs ([#549](https://github.com/joshka/girt/pull/549))
- Read and publish explicit Git sequencer recovery ([#624](https://github.com/joshka/girt/pull/624))
- Parse and execute linear rebase todo plans ([#623](https://github.com/joshka/girt/pull/623))
- Replay commit sequences entirely in memory ([#613](https://github.com/joshka/girt/pull/613))
- Compute cherry-picks and reverts in memory ([#611](https://github.com/joshka/girt/pull/611))
- Reduce commit-graph ancestry validation cost ([#627](https://github.com/joshka/girt/pull/627))
- Record commit-graph history timing against Git ([#626](https://github.com/joshka/girt/pull/626))
- Add command-page rules and site-page examples to the docs guides ([#597](https://github.com/joshka/girt/pull/597))
- Speed up jj git clone of a local path ([#606](https://github.com/joshka/girt/pull/606))
- Trust valid commit-graphs by default ([#590](https://github.com/joshka/girt/pull/590))
- Publish full and incremental commit-graph files ([#565](https://github.com/joshka/girt/pull/565))
- Build and encode full and incremental commit-graphs ([#557](https://github.com/joshka/girt/pull/557))
- Use changed-path Bloom filters in first-parent queries ([#546](https://github.com/joshka/girt/pull/546))
- Use indexed metadata for bounded history queries ([#541](https://github.com/joshka/girt/pull/541))
- Load split commit-graph chains and verify objects ([#535](https://github.com/joshka/girt/pull/535))
- Read standalone commit-graph metadata ([#497](https://github.com/joshka/girt/pull/497))
- Group code examples into paragraphs and measure dense code ([#603](https://github.com/joshka/girt/pull/603))
- Publish Linux jj git init --colocate on girt 0.14.0 (parity, no longer a timeout) ([#609](https://github.com/joshka/girt/pull/609))
- Route merges through per-path drivers ([#580](https://github.com/joshka/girt/pull/580))
- Compare merge lines with whitespace policy ([#570](https://github.com/joshka/girt/pull/570))
- Publish merge conflicts through guarded checkout ([#566](https://github.com/joshka/girt/pull/566))
- Merge commits with recursive virtual bases ([#554](https://github.com/joshka/girt/pull/554))
- Merge renamed paths without writing objects ([#544](https://github.com/joshka/girt/pull/544))
- Select merge drivers from attributes ([#540](https://github.com/joshka/girt/pull/540))
- Merge tree entries without a working tree ([#537](https://github.com/joshka/girt/pull/537))
- Add three-way content merging ([#522](https://github.com/joshka/girt/pull/522))
- Publish Linux jj log ancestry on the fixed adapter (1.01x, no longer a timeout) ([#601](https://github.com/joshka/girt/pull/601))
- Audit Git transfer compatibility ([#574](https://github.com/joshka/girt/pull/574))
- Audit other Git capabilities ([#564](https://github.com/joshka/girt/pull/564))
- Audit Git diff compatibility ([#563](https://github.com/joshka/girt/pull/563))
- Audit Git working-tree compatibility ([#559](https://github.com/joshka/girt/pull/559))
- Audit config and revision compatibility ([#553](https://github.com/joshka/girt/pull/553))
- Audit Git reference compatibility ([#551](https://github.com/joshka/girt/pull/551))
- Audit Git storage compatibility ([#547](https://github.com/joshka/girt/pull/547))
- Re-measure jj log ancestry and limited on the fixed adapter (small and medium) ([#591](https://github.com/joshka/girt/pull/591))
- Re-measure Linux clone, workspace add and gc on girt 0.14.0; add fresh read copies to the harness ([#582](https://github.com/joshka/girt/pull/582))

### Changed

- `workflow::fetch` with `FetchOptions::with_reflog` follows `core.logAllRefUpdates` as `git fetch`
  does: by default it logs remote-tracking references, and logs tags and tag-namespace references
  only when they already have a reflog. It previously created a reflog for every changed reference.
- Handles from one `Repository` reuse the parsed `packed-refs` while the file keeps its identity,
  and a local fetch into a new repository no longer probes for a loose copy of every object it
  walks. A `jj git clone` of git/git is about 15% faster.
- Loose object writes compress at zlib's best-speed level, Git's default for loose objects, and
  publish with one rename that cannot replace an existing object (hard link and unlink where the
  platform or filesystem lacks that rename). Fan-out directories are created only when missing. On
  macOS, writing 16,000 trees into an empty store takes about 290 µs per object instead of about
  790 µs, and a `jj status` that snapshots 10,000 new files in git/git took 2.2 s instead of 3.9 s
  in one heavily loaded run.

## [0.15.0](https://github.com/joshka/girt/compare/girt-v0.14.0...girt-v0.15.0) - 2026-10-18

### Other

- Consolidate packs during gc once they exceed gc.autoPackLimit ([#479](https://github.com/joshka/girt/pull/479))
- Add a How Git works guide to the site ([#536](https://github.com/joshka/girt/pull/536))
- Add CLI command references in the right margin ([#491](https://github.com/joshka/girt/pull/491))
- Re-measure the worst medium jj rows on girt 0.14.0; mark util gc as not like-for-like ([#470](https://github.com/joshka/girt/pull/470))
- Re-measure jj tree-writing rows on girt 0.13.1 (small and medium) ([#467](https://github.com/joshka/girt/pull/467))

### Behaviour changes

- `gc` now consolidates packs into one when the repository has more than `gc.autoPackLimit` packs
  (Git's key and default, 50). Set `gc.autoPackLimit = 0`, or pass the gc option that skips
  consolidation, to keep the previous additive-only behaviour. Consolidation runs only on an explicit
  gc call, never during other operations.

## [0.14.0](https://github.com/joshka/girt/compare/girt-v0.13.1...girt-v0.14.0) - 2026-10-18

### Other

- Re-measure the jj status rows on girt main with #452 ([#459](https://github.com/joshka/girt/pull/459))
- Refresh the jj benchmark matrix on girt 0.11.1 with per-cell version stamps ([#454](https://github.com/joshka/girt/pull/454))
- Apply diff attributes to patches and blame ([#463](https://github.com/joshka/girt/pull/463))

### Added

- Read `.gitattributes` with `attributes::Attributes` and `Repository::attributes`, and apply
  `binary`, `-diff`, `diff=<driver>`, `diff.<driver>.binary` and function-name patterns to patches
  with `patch::DiffAttributes`. `Repo::write_patch` applies them by default; `Repo::blame` refuses
  textconv drivers.
- Consolidate packs during `Repository::collect_garbage` and `Repository::run_maintenance` once the
  pack count exceeds `gc.autoPackLimit`, with `retention::PackConsolidation` and
  `RetentionPolicy::with_pack_consolidation` to force or skip it, `Repository::collect_garbage_with`
  to bound the new pack, `MaintenanceReport::consolidated` and `MaintenanceCause::Config`.

### Changed

- `Objects::write_patch_with` takes an optional `&mut DiffAttributes` after its options.

## [0.13.1](https://github.com/joshka/girt/compare/girt-v0.13.0...girt-v0.13.1) - 2026-10-18

### Other

- Reuse validated cache-tree subtrees in write_tree ([#458](https://github.com/joshka/girt/pull/458))
- Speed up checkout and local clone validation ([#457](https://github.com/joshka/girt/pull/457))
- Gate macOS/Linux-only status helpers so Windows builds stay warning-free ([#465](https://github.com/joshka/girt/pull/465))
- Make status trust index stat data and walk the tree in parallel ([#452](https://github.com/joshka/girt/pull/452))
- Compact ambiguous content diff boundaries ([#451](https://github.com/joshka/girt/pull/451))

### Changed

- Compact ambiguous content changes and enable indent-aware placement in content diffs, patches and
  blame.
  Add content and patch options to disable indent scoring while retaining compaction.

## [0.13.0](https://github.com/joshka/girt/compare/girt-v0.12.0...girt-v0.13.0) - 2026-10-17

### Other

- Pair empty files by default in rewrite detection ([#449](https://github.com/joshka/girt/pull/449))
- Name reftable tables the way Git's strict fsck accepts ([#447](https://github.com/joshka/girt/pull/447))
- Fix Repo CI targets and guide links ([#448](https://github.com/joshka/girt/pull/448))
- Rename the convenience handle to Repo ([#446](https://github.com/joshka/girt/pull/446))
- Add recorded identity date formatting ([#445](https://github.com/joshka/girt/pull/445))
- Add bounded line attribution with rename following ([#444](https://github.com/joshka/girt/pull/444))
- Add unified patch output for tree and content diffs ([#443](https://github.com/joshka/girt/pull/443))
- Add configured clone with checkout ([#438](https://github.com/joshka/girt/pull/438))
- Accept configured Windows drive remotes ([#437](https://github.com/joshka/girt/pull/437))
- Add a trusted repository convenience handle ([#432](https://github.com/joshka/girt/pull/432))
- Add ignore-aware status and repository ignore rules ([#434](https://github.com/joshka/girt/pull/434))

### Breaking changes

- Pair empty files by default in rewrite detection, matching `git diff -M` and `git diff-tree -r -M`.
  Empty candidates also count toward the edited-content candidate limit. Use
  `rewrites::Options::default().with_track_empty(false)` to restore the previous policy.

## [0.12.0](https://github.com/joshka/girt/compare/girt-v0.11.3...girt-v0.12.0) - 2026-10-17

### Other

- Align tree diff names with default-limit APIs ([#430](https://github.com/joshka/girt/pull/430))
- Add reference helpers and repository-format object IDs ([#429](https://github.com/joshka/girt/pull/429))
- Rename cfg-gated test calls to the _with forms ([#431](https://github.com/joshka/girt/pull/431))
- Add snapshot and commit builders ([#424](https://github.com/joshka/girt/pull/424))
- Speed up commit walks with pack read-ahead and cheaper commit parsing ([#427](https://github.com/joshka/girt/pull/427))
- Take defaults with short names; move explicit limits to _with ([#421](https://github.com/joshka/girt/pull/421))
- Add a crate-level error and Git display helpers ([#423](https://github.com/joshka/girt/pull/423))
- Skip compression when a loose object already exists ([#401](https://github.com/joshka/girt/pull/401))

## [0.11.3](https://github.com/joshka/girt/compare/girt-v0.11.2...girt-v0.11.3) - 2026-10-17

### Other

- Add current and configured signature constructors ([#410](https://github.com/joshka/girt/pull/410))
- Fix clone's 3 GB memory peak by queuing each object once in local fetch ([#402](https://github.com/joshka/girt/pull/402))
- Cut local push peak memory by releasing the source snapshot after preparing ([#399](https://github.com/joshka/girt/pull/399))
- Speed up GC's retention walk by queuing each object once ([#397](https://github.com/joshka/girt/pull/397))
- Let writers skip content the object store already holds (Objects::contains) ([#396](https://github.com/joshka/girt/pull/396))
- Measure everyday task ergonomics ([#415](https://github.com/joshka/girt/pull/415))

## [0.11.2](https://github.com/joshka/girt/compare/girt-v0.11.1...girt-v0.11.2) - 2026-10-17

### Other

- Move the cargo-deny, release-plz and Vale configs into .config ([#400](https://github.com/joshka/girt/pull/400))
- Load Git's default configuration sources in one library call ([#395](https://github.com/joshka/girt/pull/395))
- Box the loose-object scan's directory iterator to fix Windows clippy ([#394](https://github.com/joshka/girt/pull/394))

## [0.11.1](https://github.com/joshka/girt/compare/girt-v0.11.0...girt-v0.11.1) - 2026-10-17

### Other

- Fix the main build: report index errors from IndexedPack::contains ([#388](https://github.com/joshka/girt/pull/388))
- Release girt locks on Windows through verified file handles ([#377](https://github.com/joshka/girt/pull/377))
- Contain Windows HTTP fixtures in kill-on-close jobs and document fixture ownership ([#376](https://github.com/joshka/girt/pull/376))
- Say cargo add girt instead of pinning a version in the README ([#380](https://github.com/joshka/girt/pull/380))
- List packs and enumerate objects through Objects; add girt object list and count ([#368](https://github.com/joshka/girt/pull/368))
- Map pack indexes and check them lazily, as Git does ([#382](https://github.com/joshka/girt/pull/382))
- Box the revision test helper's error to fix Windows clippy ([#383](https://github.com/joshka/girt/pull/383))
- Add shell completions and man pages to the girt CLI ([#374](https://github.com/joshka/girt/pull/374))

## [0.11.0](https://github.com/joshka/girt/compare/girt-v0.10.3...girt-v0.11.0) - 2026-10-17

### Other

- Honor CLI color settings and style human output ([#379](https://github.com/joshka/girt/pull/379))
- Support SSH remotes in the CLI ([#361](https://github.com/joshka/girt/pull/361))
- Parse and resolve Git revisions and ranges ([#369](https://github.com/joshka/girt/pull/369))
- [**breaking**] pass approved SSH arguments as OsString ([#351](https://github.com/joshka/girt/pull/351))
- [**breaking**] stop exporting the loose-object error as girt::Error ([#350](https://github.com/joshka/girt/pull/350))
- [**breaking**] name options setters with the with_ prefix ([#349](https://github.com/joshka/girt/pull/349))
- Keep every live pack index reusable, however many packs are open ([#370](https://github.com/joshka/girt/pull/370))
- Show live CLI operation progress ([#373](https://github.com/joshka/girt/pull/373))
- Support abbreviated object IDs in the library and CLI ([#359](https://github.com/joshka/girt/pull/359))
- Compute tree identities from Git index entries ([#345](https://github.com/joshka/girt/pull/345))
- Simplify the first-read docs with RefName::HEAD, Repository::head and Objects::read_commit ([#356](https://github.com/joshka/girt/pull/356))
- Support disposable index cache extensions ([#358](https://github.com/joshka/girt/pull/358))
- Polish the girt CLI: color-eyre, tracing, colored help, publishing and a site page ([#362](https://github.com/joshka/girt/pull/362))
- Give ObjectKind its home in the object module ([#353](https://github.com/joshka/girt/pull/353))
- Publish SSH fixture PID atomically ([#346](https://github.com/joshka/girt/pull/346))
- Pair unique basenames before rename limits ([#348](https://github.com/joshka/girt/pull/348))
- Distinguish status byte limit failures ([#347](https://github.com/joshka/girt/pull/347))
- Deduplicate native local fetch wants ([#344](https://github.com/joshka/girt/pull/344))
- Add a girt command-line tool with Git-compatible output ([#355](https://github.com/joshka/girt/pull/355))
- cancel superseded validation runs ([#340](https://github.com/joshka/girt/pull/340))
- Fix main validation regressions ([#343](https://github.com/joshka/girt/pull/343))

### Added

- `girt::object::ObjectKind` is the canonical path for object kinds, beside `ObjectId` and
  `ObjectFormat`. `girt::ObjectKind` and `girt::tag::ObjectKind` remain as re-exports.

### Breaking changes

- Builder setters on `InitOptions`, `workflow::FetchOptions` and `workflow::PushOptions` take the
  `with_` prefix used by every other girt options type: `with_object_format`,
  `with_reference_backend`, `with_initial_branch`, `with_depth`, `with_prune`,
  `with_tag_namespace`, `with_reflog`, `with_compression`, `with_identity` and `with_push_option`.
  Behavior is unchanged; rename the calls. `with_tag_namespace` and `with_push_option` still add
  one value per call.
- The crate root no longer re-exports `loose::Error` as `girt::Error`. That name read as a
  crate-wide error type, but it only covers loose-object storage. Import `girt::loose::Error`
  instead; the type itself is unchanged.
- `transport::ssh::ApprovedSshCommand::arguments` is `Vec<OsString>`, as
  `remote::CredentialProgram::arguments` already was, so an approved key path that is not UTF-8
  reaches the SSH command unchanged. `new` and `with_arguments` take `Vec<OsString>`; existing
  `"…".into()` arguments still compile. The empty, 8 KiB and control-character checks are unchanged
  for Unicode arguments and reject ASCII control bytes in other arguments.

## [0.10.3](https://github.com/joshka/girt/compare/v0.10.2...v0.10.3) - 2026-10-17

### Other

- Teach configuration provenance on the website ([#308](https://github.com/joshka/girt/pull/308))
- complete diff and rewrite workflows ([#303](https://github.com/joshka/girt/pull/303))
- explain status, checkout and ignore decisions ([#298](https://github.com/joshka/girt/pull/298))
- complete index lifecycle examples and recovery ([#292](https://github.com/joshka/girt/pull/292))
- Teach the fetch, push and clone lifecycles in their module docs ([#283](https://github.com/joshka/girt/pull/283))
- Explain transports, limits and trust in the transport docs ([#279](https://github.com/joshka/girt/pull/279))
- Teach bounded reference and reftable workflows ([#296](https://github.com/joshka/girt/pull/296))
- Teach configuration provenance and policy ([#294](https://github.com/joshka/girt/pull/294))
- Teach repository snapshots and initialization ([#291](https://github.com/joshka/girt/pull/291))
- Explain object storage policies and recovery ([#289](https://github.com/joshka/girt/pull/289))
- Teach object views and error recovery ([#287](https://github.com/joshka/girt/pull/287))
- Publish a project website on GitHub Pages ([#271](https://github.com/joshka/girt/pull/271))
- Keep a destination reader alive across local push phases ([#250](https://github.com/joshka/girt/pull/250))
- Keep pack index entry tables at 8 bytes per entry ([#249](https://github.com/joshka/girt/pull/249))
- Explain remotes, refspecs and credentials in the remote docs ([#277](https://github.com/joshka/girt/pull/277))
- Teach configured-remote transfers in the workflow docs ([#272](https://github.com/joshka/girt/pull/272))
- Reflow benchmark publication instructions ([#281](https://github.com/joshka/girt/pull/281))
- Teach retention and maintenance safety ([#270](https://github.com/joshka/girt/pull/270))
- Teach configuration resolution, interpretation, and editing ([#265](https://github.com/joshka/girt/pull/265))
- Teach references and reflog publication ([#262](https://github.com/joshka/girt/pull/262))
- Teach objects, storage, and history with runnable examples ([#260](https://github.com/joshka/girt/pull/260))
- Teach crate orientation and repository discovery ([#258](https://github.com/joshka/girt/pull/258))
- Teach tree diffs and bounded rewrite detection ([#264](https://github.com/joshka/girt/pull/264))
- Teach staging and raw working-tree workflows ([#257](https://github.com/joshka/girt/pull/257))
- Publish the jj operation benchmark matrix ([#278](https://github.com/joshka/girt/pull/278))
- Share validated pack indexes of any size between overlapping readers ([#248](https://github.com/joshka/girt/pull/248))
- Walk new commits before their trees in native local fetch ([#251](https://github.com/joshka/girt/pull/251))
- adopt start-and-repair readiness and repair reporting ([#268](https://github.com/joshka/girt/pull/268))
- publish after an independent Linux check ([#267](https://github.com/joshka/girt/pull/267))

## [0.10.2](https://github.com/joshka/girt/compare/v0.10.1...v0.10.2) - 2026-10-16

### Other

- Open Git-scale local push destinations under caller pack limits ([#243](https://github.com/joshka/girt/pull/243))
- Count rewrite candidates for the limit as Git does ([#238](https://github.com/joshka/girt/pull/238))
- Open Git-scale local fetch sources under caller pack limits ([#237](https://github.com/joshka/girt/pull/237))

### Breaking changes

- The rewrite candidate limit counts files as Git does: binary files count even when
  `approximate_binary` is false, and empty files count when `track_empty` is true. Inputs near the
  limit with such files can now be skipped (`Error::Candidates` from `detect_rewrites`) where they
  were previously scored.

### Fixed

- Native local push accepts Git-scale destinations, such as a Linux clone with a 300 MB pack
  index. The destination object store opens under the new `PushLimits::destination_packs`
  (`PackLimits::trusted()` in `PushLimits::trusted()`) to check history and install the pushed
  pack, rather than a fixed 64 MiB index bound.
- Native local fetch and clone accept Git-scale sources, such as a Linux clone with a 300 MB pack
  index. The source object store opens under the new `FetchLimits::source_packs`
  (`PackLimits::trusted()` in `FetchLimits::trusted()`), the received index is bounded through
  `max_objects` rather than a fixed 64 MiB, and clone packs references under its snapshot limits.
  A source store that exceeds its limits is reported as `FetchError::SourceObjects`, not as a
  destination failure.

## [0.10.1](https://github.com/joshka/girt/compare/v0.10.0...v0.10.1) - 2026-10-16

### Other

- prioritize fast PR feedback ([#235](https://github.com/joshka/girt/pull/235))

## [0.10.0](https://github.com/joshka/girt/compare/v0.9.2...v0.10.0) - 2026-10-16

### Other

- Keep exact rewrites when the candidate limit skips edited detection ([#230](https://github.com/joshka/girt/pull/230))
- [**breaking**] consolidate native push entries ([#214](https://github.com/joshka/girt/pull/214))
- [**breaking**] consolidate native fetch entries ([#213](https://github.com/joshka/girt/pull/213))
- [**breaking**] remove transfer constructor aliases ([#212](https://github.com/joshka/girt/pull/212))
- [**breaking**] move orchestration into workflow ([#209](https://github.com/joshka/girt/pull/209))
- [**breaking**] replace API policy booleans ([#208](https://github.com/joshka/girt/pull/208))
- [**breaking**] type maintenance failure causes ([#205](https://github.com/joshka/girt/pull/205))
- Match Git at the loose prune cutoff ([#184](https://github.com/joshka/girt/pull/184))

### Added

- `Objects::detect_rewrites_or_exact` returns exact rename and copy matches with an explicit
  `InexactSkipped` report when the candidate limit skips edited-content detection, as Git reports
  exact renames when it skips rename detection for too many files.

### Changed

- `Options::candidate_limit` now skips edited detection only when remaining sources times remaining
  destinations exceeds the squared limit, the bound Git applies for `diff.renameLimit`; the
  candidate counts can still differ from Git's (see `Options::candidate_limit`). Calls that previously
  succeeded return the same result; some that returned `Error::Candidates`, such as a few sources
  against many new files, now complete.

### Breaking changes for 0.10.0

- Represent `RetentionPolicy::recent_cutoff` as `Option<SystemTime>`; `None` disables age-based
  reclamation and is the default, retaining even epoch-zero loose objects. Existing
  `with_recent_cutoff(time)` calls keep their behavior; direct field assignments use `Some(time)`.
  Explicit loose expiry now includes equality with the cutoff, matching Git's prune boundary.
- Maintenance `Incomplete` payloads now carry `retention::RetentionError`, including retention
  planning, repacking, pruning, reflog expiry and pack retirement. jj adapters should use
  `RetentionError::kind()` for recovery categories and match detailed conditions when needed, rather
  than parse messages. Read failures retain the original error through `Error::source`; render the
  chain once. `ExpireCause::Incomplete` boxes its detail to keep expiry failures compact on Windows;
  use `detail.as_ref()` to borrow it or `*detail` to take ownership. Its `Error::source()` downcasts to
  `Box<RetentionError>`. Plans no longer promise `Clone`
  or equality for errors containing I/O causes; use `is_complete()` or
  match `RetentionOutcome::Complete`. Retained source errors also remove the `UnwindSafe` and
  `RefUnwindSafe` guarantees from plans; `Send + Sync` remain supported. Callers catching panics
  must select their own recovery boundary. Partial reports and deletion preconditions are unchanged.
- Ignore queries take `ignore::InputKind::{File, Directory}`. Advertisement constructors and
  records use `fetch::AdvertisementKind::{Tip, PeeledHint}` through `kind`/`with_kind`; only tips
  remain selectable wants. Reference edits use `refs::Dereference::{Stored, Follow}`. jj should
  select `Stored` for its conditional stored-name edits and derive `InputKind` from traversal.
- `remote::Mapping::force` uses `ForceIntent::{Normal, Requested}`; a request still needs separate
  publication authorization. `ProtocolEnvironment::from_user`/`with_from_user` become
  `origin`/`with_origin` with `ProtocolOrigin::{User, Automatic}`; `None` retains the user default.
  Existing named bit setters and `push::ForcePolicy` are unchanged.
- Configured-remote convenience APIs move from `transfer` to `workflow`. Replace
  `repository.fetch(...)`, `repository.push(...)` and `repository.remote_head(...)` with
  `workflow::fetch(&repository, ...)`, `workflow::push(&repository, ...)` and
  `workflow::remote_head(&repository, ...)`. Import environment, callbacks, options and outcomes
  from `girt::workflow`. jj's synchronous adapter keeps the same refspec, credential, runtime and
  automatic-maintenance behavior. Core fetch/push plans and artifacts retain their namespaces.
- `KnownHistory::new_local` is removed in favor of `new`. `PreparedPush::new` now takes explicit
  receiver roots (`&[]` for a full transfer), replacing `new_excluding` and `new_local`.
  `new_local_with_progress` becomes `new_with_progress`. jj adapters select receiver roots
  explicitly; preparation, validation, cancellation and cleanup contracts are unchanged.
- Native local fetch has one `receive_local(source, select, request, control, observer)` entry.
  Use `LocalFetchRequest::new(limits).with_known(&history)` for incremental transfer and
  `LocalFetchObserver` for messages, construction and validation. `FetchRequest::receive_local`
  takes that same observer; its separate progress overloads are removed. jj's high-level workflow
  adapter needs no additional change for these lower-level calls. Transfer, validation, cleanup
  and publication boundaries are unchanged.
- Native local push has one `send_local(destination, request, control, validation)` entry.
  Construct `LocalPushRequest::new(&prepared)` and set inherited configuration or reflog identity
  rather than select an overload. The public `LocalPushContext` is removed; the request owns both inputs.
  jj's configured workflow adapter keeps its existing receiver configuration and identity policy.
  Preparation, validation, cancellation, cleanup and partial effects are unchanged. P3, P5 and P6
  remain deferred; P1 requires no additional changes in those areas.

## [0.9.2](https://github.com/joshka/girt/compare/v0.9.1...v0.9.2) - 2026-10-16

### Other

- Gate API breaks before merging pull requests ([#206](https://github.com/joshka/girt/pull/206))
- Restore unwind safety for public cache owners ([#204](https://github.com/joshka/girt/pull/204))
- Shorten Windows CI with parallel feature lanes ([#200](https://github.com/joshka/girt/pull/200))
- stop fetch validation at a complete parent's tree ([#193](https://github.com/joshka/girt/pull/193))

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
