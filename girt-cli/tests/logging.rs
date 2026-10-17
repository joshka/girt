//! The CLI takes its tracing filter from `GIRT_LOG` and ignores `RUST_LOG`.
//!
//! girt records spans rather than events, so a debug filter shows up as the span trace in an error
//! report.

use std::process::{Command, Output};

/// Runs a command that fails outside a repository, with `name=value` as the only log setting.
fn girt_failing_with(name: &str, value: &str) -> Output {
    let directory = tempfile::tempdir().expect("create scratch directory");
    Command::new(env!("CARGO_BIN_EXE_girt"))
        .args(["object", "show", "HEAD"])
        .current_dir(directory.path())
        .env_remove("GIRT_LOG")
        .env_remove("RUST_LOG")
        .env("GIT_CEILING_DIRECTORIES", directory.path())
        .env(name, value)
        .output()
        .expect("run girt")
}

#[test]
fn girt_log_sets_the_filter() {
    let output = girt_failing_with("GIRT_LOG", "debug");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SPANTRACE"), "{stderr}");
}

#[test]
fn rust_log_is_ignored() {
    let output = girt_failing_with("RUST_LOG", "debug");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("SPANTRACE"), "{stderr}");
}

#[test]
fn invalid_girt_log_is_an_error() {
    let output = girt_failing_with("GIRT_LOG", "girt=[");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid GIRT_LOG"), "{stderr}");
}
