# List available recipes.
default:
    @just --list

# Run the common checks for Rust implementation changes.
check: fmt-check test clippy rustdoc docs-rs guide-examples

# Run unit, integration, and documentation tests.
test:
    cargo test --all-features
    cargo test -p girt-cli

# Measure object and repository baselines (not a CI performance gate).
bench:
    cargo bench --bench object_formats --bench peeling --bench blobs --bench trees --bench loose_trees --bench commits --bench tags --bench repositories --bench references --bench packs --bench history --bench pack_write --bench fetch --bench push --bench remotes --bench fetch_workflow --bench clone --bench tree_compare --bench content_diff --bench index --bench status --bench checkout

# Reject Clippy warnings across all targets.
clippy:
    cargo clippy --all-features --all-targets -- -D warnings
    cargo clippy -p girt-cli --all-targets -- -D warnings

# Check stable Rustdoc with all features and with optional transports absent.
rustdoc:
    RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked --all-features --no-deps --document-private-items
    RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked --no-default-features --no-deps

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

# Check formatting and calibrated prose rules for the reader-facing guides.
docs-check: fmt-md-check
    vale --config .config/vale.ini README.md CONTRIBUTING.md docs/configuration.md docs/reference-transactions.md docs/repositories.md docs/http.md docs/ssh.md docs/working-tree.md

# Measure the quality scorecard (docs/quality.md); pass --full to remeasure coverage and mutants.
scorecard *args:
    scripts/quality-scorecard.py {{args}}

# Compile and run the standalone Rust programs in the developer website guides.
guide-examples:
    python3 scripts/check-guide-examples.py
