# List available recipes.
default:
    @just --list

# Run the common checks for Rust implementation changes.
check: fmt-check test clippy docs-rs

# Run unit, integration, and documentation tests.
test:
    cargo test --all-features

# Measure object and repository baselines (not a CI performance gate).
bench:
    cargo bench --bench blobs --bench trees --bench loose_trees --bench commits --bench tags --bench repositories --bench references --bench packs --bench history --bench pack_write --bench fetch --bench push --bench remotes --bench fetch_workflow --bench clone --bench tree_compare --bench content_diff --bench index

# Reject Clippy warnings across all targets.
clippy:
    cargo clippy --all-features --all-targets -- -D warnings

# Check documentation with docs.rs options and reject Rustdoc warnings.
docs-rs:
    RUSTDOCFLAGS="-D warnings" cargo +nightly docs-rs

# Format Rust and Markdown.
fmt: fmt-rust fmt-md

# Check Rust and Markdown formatting.
fmt-check: fmt-rust-check fmt-md-check

# Format Rust using the nightly-only options in rustfmt.toml.
fmt-rust:
    cargo +nightly fmt --all

# Check formatting using the same toolchain as fmt.
fmt-rust-check:
    cargo +nightly fmt --all -- --check

# Wrap Markdown prose and align table columns and separators.
fmt-md:
    rumdl fmt .

# Check Markdown without changing files.
fmt-md-check:
    rumdl check .
