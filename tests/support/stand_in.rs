//! Generated test programs launched through one checked-in executable, `tests/fixtures/stand-in`.
//!
//! macOS checks each new executable file on its first launch. The checks run one at a time and
//! take seconds while builds create new binaries, so a fresh script per test spends its transport
//! deadline waiting. A link to the stand-in reuses one checked file; the program itself is data.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Once;

const STAND_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stand-in");

/// Creates `path` as a program that runs `body` with `/bin/sh`.
pub fn shell(path: &Path, body: &str) {
    install(path, "sh", body);
}

/// Creates `path` as a program that runs `body` with the `python3` found on its `PATH`.
pub fn python(path: &Path, body: &str) {
    install(path, "py", body);
}

fn install(path: &Path, extension: &str, body: &str) {
    let mut program = path.as_os_str().to_owned();
    program.push(".");
    program.push(extension);
    std::fs::write(program, body).unwrap();
    std::os::unix::fs::symlink(STAND_IN, path).unwrap();
    // Take any first-launch check now, outside the deadline of the operation under test.
    static LAUNCHED: Once = Once::new();
    LAUNCHED.call_once(|| {
        let _ = Command::new(STAND_IN)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}
