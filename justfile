# List available recipes.
default:
    @just --list

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
