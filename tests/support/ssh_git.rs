//! Isolated sshd process, disposable keys/config/trust, and exact forced service/repository
//! boundary.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use girt::transport::ssh::SshRemote;

pub struct Server {
    child: Child,
    pub root: PathBuf,
    pub port: u16,
    user: String,
    repository: String,
}
impl Server {
    pub fn new(repository: &Path, fault: &str) -> Self {
        let mut child = Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ssh/server.py"
            ))
            .arg(repository)
            .args(["--fault", fault])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        // The fixture's tiny JSON object contains generated paths and a validated local username.
        // Avoid adding a JSON dependency solely for this test protocol.
        let fields: Vec<_> = line
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
            .map(|s| s.split_once(':').unwrap().1.trim().trim_matches('"'))
            .collect();
        let port = fields[0].parse().expect("sshd fixture startup");
        let user = fields[1].to_owned();
        let root = PathBuf::from(fields[2]);
        Self {
            child,
            root,
            port,
            user,
            repository: repository.to_str().unwrap().to_owned(),
        }
    }
    pub fn remote(&self, config: &str) -> SshRemote {
        SshRemote::new(
            "127.0.0.1",
            &self.user,
            self.port,
            &self.repository,
            Path::new("/usr/bin/ssh"),
            &self.root.join(config),
        )
        .unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.wait();
    }
}
