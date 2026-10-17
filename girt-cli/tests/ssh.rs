//! Original Git-generated repositories served by the library's disposable loopback sshd.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[test]
fn ssh_clone_heads_fetch_and_push() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ssh.py"
        ))
        .arg(env!("CARGO_BIN_EXE_girt"))
        .output()
        .expect("run SSH CLI interoperability fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
