//! Loopback process ownership shared by HTTP integration tests, example and benchmark.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub struct Server {
    child: Child,
    pub url: String,
    root: tempfile::TempDir,
}
impl Server {
    pub fn new(
        repository: &Path,
        fault: &str,
        authorization: &str,
        tls: Option<(&Path, &Path)>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("python3");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/http/server.py"
            ))
            .arg(repository)
            .args(["--fault", fault, "--authorization", authorization])
            .arg("--requests")
            .arg(root.path().join("requests"));
        if let Some((cert, key)) = tls {
            command.arg("--certificate").arg(cert).arg("--key").arg(key);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut port = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        let port: u16 = port.trim().parse().expect("fixture server startup");
        Self {
            child,
            url: format!(
                "{}://127.0.0.1:{port}/repo",
                if tls.is_some() { "https" } else { "http" }
            ),
            root,
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
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
