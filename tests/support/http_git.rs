//! Loopback process ownership shared by HTTP integration tests, example and benchmark.
use std::path::Path;
use std::process::Command;

#[path = "fixture_process.rs"]
pub(super) mod fixture_process;
use fixture_process::Process;

pub struct Server {
    _process: Process,
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
        Self::spawn(repository, fault, authorization, tls, None)
    }
    /// An HTTPS server that requires client certificates issued by `client_ca`.
    #[allow(dead_code)]
    pub fn requiring_client_certificate(
        repository: &Path,
        tls: (&Path, &Path),
        client_ca: &Path,
    ) -> Self {
        Self::spawn(repository, "", "", Some(tls), Some(client_ca))
    }
    fn spawn(
        repository: &Path,
        fault: &str,
        authorization: &str,
        tls: Option<(&Path, &Path)>,
        client_ca: Option<&Path>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/http/server.py"
            ))
            .arg(repository)
            .args(["--fault", fault, "--authorization", authorization])
            .arg("--requests")
            .arg(root.path().join("requests"))
            .arg("--owner")
            .arg(std::process::id().to_string());
        if let Some((cert, key)) = tls {
            command.arg("--certificate").arg(cert).arg("--key").arg(key);
        }
        if let Some(client_ca) = client_ca {
            command.arg("--client-ca").arg(client_ca);
        }
        let mut process = Process::spawn_grouped(&mut command, false);
        let port: u16 = process.ready(|line| line.trim().parse::<u16>().map_err(|e| e.to_string()));
        Self {
            _process: process,
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
