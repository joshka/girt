# Publication and Maintenance Plan

The release must support the tested jj integration through published crates, with useful docs.rs
pages. R35 remains the integration owner; R42–R45 turn its verified scope into a maintained release.
Optional limitations stay on the roadmap. Unsafe mutation, corruption and required interoperability
failures cannot be converted into release acceptance by documenting them.

## Authorization and Sequence

The maintainer authorizes management, publication and updates of `girt*` crates on crates.io and
girt resources on GitHub, including trusted publishing settings. Cargo credentials are already
configured; never print or copy them into reports. This authorization does not include submitting
pull requests to jj. Local jj integration and validation remain authorized.

First qualify and publish the initial crates, then adopt release-plz for subsequent releases. Do not
publish a placeholder to satisfy the order. Registry publication must use the tested package
contents and an honest supported-scope statement. Keep unrelated crates and repositories untouched.

## Maintainer Policy

Use jj's existing MSRV policy for the integration target; otherwise support stable Rust N-2 (two
stable releases behind current). Resolve and test the concrete compiler version during release
qualification. Use ordinary Rust/Cargo 0.x compatibility conventions, without a bespoke versioning
policy.

Use GitHub's standard security settings and reporting facilities. Keep the requested zizmor,
cargo-deny and Dependabot checks; do not add a custom security-process project. Handle yanking
manually if needed. Partial-publication and release-recovery procedures are not prerequisites.

General performance parity with Git remains the goal under
[A20](jj-acceptance.md#a20--representative-git-parity-performance). Report workload-specific gaps
and retain their roadmap owners; a deferred optimization does not lower the goal. Long-horizon
fuzzing is tracked as [B10](jj-roadmap.md#b10--long-horizon-fuzzing).

## R42 — Documentation Quality

Apply the [documentation standard](documentation.md) and [Rustdoc standard](rustdoc.md) to every
human-facing surface. Begin with a reader/task inventory and a few representative before/after
revisions; use them to refine the voice before broad edits. Apply Diátaxis to information ownership,
not as a mandatory directory layout. Integrate a calibrated Vale configuration and terminology
vocabulary with the existing rumdl formatting checks.

Acceptance: the first-use, reference/error-recovery and contributor journeys work from both README
and docs.rs entry points. Examples compile and run with documented features; links and feature/MSRV
claims are checked; rendered navigation works for sequential reading, search and scanning. Audit all
surfaces, resolve substantive findings and retain a concise report of decisions and remaining
owners. Do not replace editorial evaluation with lint scores or a large volume of new prose.

## R43 — Release Qualification and CI

Inventory the actual publishable crates; avoid unnecessary splitting. Verify versions, licensing
files and provenance, metadata, feature combinations, MSRV, supported targets and public API
stability. Inspect package contents for machine-local paths, missing assets and accidental fixtures.
Build/test extracted packages and consumer examples without local workspace patches. Reconcile known
failures and limitations against the supported release scope.

Configure [zizmor](https://docs.zizmor.sh/) for workflow analysis and
[cargo-deny](https://embarkstudios.github.io/cargo-deny/checks/index.html) for licenses, advisories,
bans and sources. Review findings instead of suppressing them wholesale. Use narrowly scoped
workflow permissions and pinned dependencies/actions with an update path. Keep publishing
credentials unavailable to untrusted pull-request code. Validate workflow configuration and ensure
required jobs actually execute.

Configure
[Dependabot](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference)
for Cargo and GitHub Actions with explicit cooldown and grouping. Start with weekly routine updates
and a seven-day version-update cooldown where supported; group compatible Cargo updates and Actions
separately, keeping breaking changes separate. Prefer lockfile refreshes over gratuitous manifest
minimum bumps. Security updates must remain timely; version cooldown does not apply to them. Verify
the current schema and behavior during implementation.

Acceptance includes a release-candidate package dry run, dependency/runtime subprocess audit,
consumer tests at recorded revisions, and a short supported-platform/feature matrix. Verify the
chosen MSRV and standard GitHub security settings; avoid adding bespoke release-process gates.

## R44 — Initial Publication

Publish necessary crates in dependency order using the existing Cargo login after R43 passes. Record
package versions and source revisions. Verify registry availability, install a fresh consumer
without path patches, and switch the local jj integration to the published version for relevant
acceptance tests. Account for any permitted remote servers and extension programs in the audit.

Verify successful [docs.rs builds](https://docs.rs/about/builds) for the published versions and
inspect their landing pages, links, examples and feature/platform annotations. A local Rustdoc build
alone does not complete this step. Record failures, fix them through a new release where necessary,
and never claim a pending docs.rs build succeeded. Completion requires usable registry artifacts and
hosted documentation, plus release notes describing behavior and known limitations.

## R45 — Release Automation

After R44, install/configure [release-plz](https://release-plz.dev/docs/github/quickstart) for
release PRs and subsequent publication. Configure trusted publishers for each crate and
least-privilege OIDC permissions on the publishing job. The documented flow obtains registry
credentials itself; do not add redundant token-exchange actions or persistent registry secrets.
Recheck upstream guidance when implementing. The initial manual release precedes this setup,
consistent with the documented first-publication restriction.

Acceptance: dry runs select only intended crates, release PR version/changelog changes are correct,
and permissions and workflow triggers are reviewed with zizmor. Verify registry publisher bindings
and the release workflow's controlled path without publishing an artificial version just to test it.
Apply ordinary Rust 0.x versioning. Record that first live automated publication remains unproven
until a genuine release exercises it.
