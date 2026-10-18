//! Git remote helpers (`git-remote-<name>`) that offer the `connect` capability.
//!
//! girt speaks only the part of the remote-helper protocol that hands over a plain connection:
//! `capabilities`, then `connect <service>`. Helpers that need girt to speak `fetch`, `push`,
//! `import` or `export` are refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{ConnectError, ConnectRequest, Connection, Transport};

/// Runs an approved `git-remote-<name>` program for each connection.
pub(crate) struct RemoteHelper {
    pub(crate) program: PathBuf,
    // Git's first argument: the remote's name, or the URL for an unnamed remote.
    pub(crate) remote: String,
    pub(crate) environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl Transport for RemoteHelper {
    fn connect(&self, request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError> {
        let url = String::from_utf8_lossy(request.url()).into_owned();
        // `<name>::<address>` passes only the address; any other URL is passed whole.
        let address = url
            .split_once("::")
            .map_or(url.as_str(), |(_, address)| address);
        let mut child = Command::new(&self.program)
            .args([self.remote.as_str(), address])
            .env_clear()
            .envs(self.environment.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(ConnectError::new)?;
        let mut input = child.stdin.take().expect("piped");
        let mut output = BufReader::new(child.stdout.take().expect("piped"));
        let handshake = (|| {
            input.write_all(b"capabilities\n")?;
            input.flush()?;
            let mut connect = false;
            for line in lines(&mut output)? {
                connect |= line.trim_start_matches('*') == "connect";
            }
            if !connect {
                return Err(ConnectError::new(
                    "remote helper does not offer `connect`, the only helper protocol girt speaks",
                ));
            }
            writeln!(input, "connect {}", request.service().name())?;
            input.flush()?;
            match read_line(&mut output)?.as_str() {
                "" => Ok(()),
                "fallback" => Err(ConnectError::new(
                    "remote helper cannot connect to this service directly",
                )),
                _ => Err(ConnectError::new(
                    "unexpected remote helper reply to `connect`",
                )),
            }
        })();
        if let Err(error) = handshake {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        // Buffered bytes beyond the handshake belong to the service.
        let buffered = output.buffer().to_vec();
        let output = output.into_inner();
        child.stdin = Some(input);
        child.stdout = Some(output);
        Ok(Box::new(Prefixed {
            prefix: std::io::Cursor::new(buffered),
            inner: super::ChildConnection::new(child),
        }))
    }
}

impl From<std::io::Error> for ConnectError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error)
    }
}

// Reads lines up to the blank line that ends a helper's capability list.
fn lines(reader: &mut impl BufRead) -> Result<Vec<String>, ConnectError> {
    let mut lines = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(lines);
        }
        if lines.len() >= 256 {
            return Err(ConnectError::new("remote helper capability list too long"));
        }
        lines.push(line);
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<String, ConnectError> {
    let mut line = Vec::new();
    reader.take(4096).read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(ConnectError::new("remote helper ended its reply early"));
    }
    line.pop();
    String::from_utf8(line).map_err(|_| ConnectError::new("remote helper reply is not UTF-8"))
}

// A connection that first returns bytes read ahead during the handshake.
struct Prefixed {
    prefix: std::io::Cursor<Vec<u8>>,
    inner: super::ChildConnection,
}

impl Read for Prefixed {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.prefix.read(buffer)? {
            0 => self.inner.read(buffer),
            read => Ok(read),
        }
    }
}

impl Write for Prefixed {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Connection for Prefixed {
    fn close_write(&mut self) -> std::io::Result<()> {
        self.inner.close_write()
    }

    fn finish(self: Box<Self>) -> Result<(), ConnectError> {
        Box::new(self.inner).finish()
    }
}
