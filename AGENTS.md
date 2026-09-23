# Repository Guidelines

## Purpose and Architecture

`girt` is an incremental, idiomatic Rust Git library intended for consumers such as jj.
Model Git's concepts directly: objects, trees, commits, references, the index, and repositories.
Supported features must preserve Git's formats and semantics exactly; reproducing CLI behavior or
another library's API is not the goal. Report unsupported cases explicitly.
Build narrow, usable increments and avoid speculative abstractions or configuration complexity.

## Independent Implementation and Licensing

Target dual MIT/Apache-2.0 licensing. Write original code, documentation, and tests. Do not copy,
translate, or adapt copyrightable expression from Git source code. Non-copyrightable ideas,
algorithms, and processes may inform the implementation. Use format specifications and observable
behavior to establish compatibility; record reference and fixture provenance. Check dependency
licenses before adding them.

## Project Structure and Module Organization

The repository currently contains one Rust 2024 library crate:

- `Cargo.toml`: package metadata and dependencies; currently no dependencies.
- `src/lib.rs`: library root and scaffold unit tests.
- `target/`: generated build output, ignored by version control.

Add modules under `src/` around coherent Git concepts. Keep unit tests beside the implementation;
add `tests/` for integration tests and `tests/fixtures/` for original compatibility fixtures.
There is currently no CLI or asset directory.

## Build, Test, and Development Commands

- `cargo build`: compile the library.
- `cargo test`: run unit, integration, and documentation tests.
- `cargo fmt --check`: check standard Rust formatting.
- `cargo clippy --all-targets -- -D warnings`: check lint warnings.
- `cargo doc --no-deps`: generate API documentation.

## Coding Style and Testing

Use rustfmt, four-space indentation, `snake_case` functions/modules, and `UpperCamelCase` types.
Prefer reader locality, cohesive types, explicit side effects, and small meaningful functions.
Document public contracts and include Rustdoc usage examples.

Use Rust's built-in test framework with descriptive names such as `rejects_truncated_object`.
Test supported behavior against Git, including malformed inputs and byte-level round trips where
applicable. Generate fixtures independently. No numerical coverage threshold is established.

## Pull Request Guidelines

PRs should explain the problem, supported behavior, compatibility evidence, validation commands,
and limitations. Link relevant issues and identify dependency or public API changes.
