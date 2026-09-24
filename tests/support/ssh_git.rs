//! Isolated sshd process, disposable keys/config/trust, and exact forced service/repository
//! boundary.
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "fixture_process.rs"]
mod fixture_process;
use fixture_process::Process;
use girt::transport::ssh::SshRemote;

pub struct Server {
    _process: Process,
    pub root: PathBuf,
    pub port: u16,
    user: String,
    repository: String,
}
impl Server {
    pub fn new(repository: &Path, fault: &str) -> Self {
        let mut command = Command::new("python3");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ssh/server.py"
            ))
            .arg(repository)
            .args(["--fault", fault]);
        let mut process = Process::spawn(&mut command, true);
        let (port, user, root) = process.ready(parse_startup);
        Self {
            _process: process,
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
fn parse_startup(line: &str) -> Result<(u16, String, PathBuf), String> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let port = value["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p != 0)
        .ok_or("invalid sshd port")?;
    let user = value["user"]
        .as_str()
        .ok_or("missing sshd user")?
        .to_owned();
    let root = PathBuf::from(value["root"].as_str().ok_or("missing sshd root")?);
    Ok((port, user, root))
}

#[cfg(test)]
mod tests {

    #[test]
    fn startup_json_preserves_path_punctuation_and_escapes() {
        use super::*;
        let (_, user, root) =
            parse_startup(r#"{"root":"/tmp/a,b:c\"d","user":"test","port":1234}"#).unwrap();
        assert_eq!(user, "test");
        assert_eq!(root, PathBuf::from("/tmp/a,b:c\"d"));
    }
}
