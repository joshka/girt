# Repository Guidelines

## Purpose and Architecture

`girt` is an incremental, idiomatic Rust Git library intended for consumers such as jj.

- Model Git's concepts directly: objects, trees, commits, references, the index, and repositories.
- Preserve Git's formats and semantics exactly for supported features. Reproducing Git's CLI
  behavior or another library's API is not the goal.
- Report unsupported cases explicitly.
- Build narrow, usable increments; avoid speculative abstractions or configuration complexity.

## Maintaining These Guidelines

- When maintainer feedback establishes a reusable rule, capture it in `AGENTS.md` or the relevant
  linked convention document as part of the work. Do not rely on conversation history alone.
- Record the general rule and its rationale; keep task-specific decisions in the change or API docs.
- Update existing guidance when possible, and keep `AGENTS.md` as the entry point to detailed
  guides.

## Independent Implementation and Licensing

- Target dual MIT/Apache-2.0 licensing. Write original code, documentation, and tests.
- Do not copy, translate, or adapt copyrightable expression from Git source code. Non-copyrightable
  ideas, algorithms, and processes may inform the implementation.
- Establish compatibility through format specifications and observable behavior; record reference
  and fixture provenance.
- Check dependency licenses before adding dependencies.

## Project Structure and Module Organization

- Add modules under `src/` around coherent Git concepts.
- For Rust design, implementation, and review, load and apply
  [Rust Conventions](docs/rust-conventions.md).
- Keep unit tests beside the implementation; use `tests/` for integration tests and
  `tests/fixtures/` for original compatibility fixtures.

## Documentation

- When writing or reviewing prose, including doc comments, load and apply
  [Documentation Standard](docs/documentation.md).
- When adding or changing public APIs, Rustdoc, or examples, also load and apply
  [Rustdoc Standard](docs/rustdoc.md).
- Apply these guides during ordinary implementation without waiting for a documentation request.

## Build, Test, and Development Commands

- `cargo build`: compile the library.
- `cargo test`: run unit, integration, and documentation tests.
- `just fmt`: format Rust with nightly rustfmt and Markdown with rumdl.
- `just fmt-check`: check formatting without changing files.
- `just fmt-md` / `just fmt-md-check`: format or check Markdown only.
- `just check`: run formatting checks, tests, Clippy, and the docs.rs check.
- `just clippy`: check all targets, rejecting warnings.
- `just docs-rs`: check docs.rs documentation builds on nightly, rejecting Rustdoc warnings.
- `cargo doc --no-deps`: generate API documentation.

## Coding Style and Testing

- If build or formatting tools are missing, follow [Setup](CONTRIBUTING.md#setup). Formatting needs
  nightly rustfmt for the unstable options in `rustfmt.toml`.
- Follow `.config/rumdl.toml`: reflow Markdown prose to 100-character lines and align table columns
  and separators; tables may exceed the prose limit.
- Use four-space indentation, `snake_case` functions/modules, and `UpperCamelCase` types.
- Prefer reader locality, cohesive types, explicit side effects, and small meaningful functions.
- Use Rust's built-in test framework with descriptive names such as `rejects_truncated_object`.
- Test supported behavior against Git, including malformed inputs and byte-level round trips where
  applicable. Generate fixtures independently. No numerical coverage threshold is established.
- For implementation requests, finish the authorized change, update affected docs, and fix failures
  caused by the change before handing it back. Report any remaining blockers.
- Use [Development Checks](CONTRIBUTING.md#development-checks) to select validation for the change.
  After checks pass, broaden or repeat them only when further edits, failures, or unresolved
  concerns justify it.

## Pull Request Guidelines

- Explain the problem, supported behavior, compatibility evidence, validation commands, and
  limitations.
- Link relevant issues and identify dependency or public API changes.
