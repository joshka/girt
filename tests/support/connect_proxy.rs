//! Fixed-target CONNECT fixture; no external network or persistent trust modification.
use std::process::Command;

use super::http_git::fixture_process::Process;

pub struct Proxy {
    _process: Process,
    root: tempfile::TempDir,
    pub url: String,
}

impl Proxy {
    pub fn new(tls_url: &str, authorization: &str) -> Self {
        let port = tls_url
            .split(':')
            .nth(2)
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/http/connect_proxy.py"
            ))
            .arg(port)
            .arg(root.path().join("requests"))
            .args(["--authorization", authorization]);
        let mut process = Process::spawn(&mut command, false);
        let port: u16 = process.ready(|line| line.trim().parse::<u16>().map_err(|e| e.to_string()));
        Self {
            _process: process,
            root,
            url: format!("http://127.0.0.1:{port}"),
        }
    }

    pub fn requests(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("requests"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}
