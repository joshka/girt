//! The long-running `filter.<name>.process` protocol (version 2).
//!
//! girt offers the `clean` and `smudge` capabilities and never `delay`, so the filter answers each
//! request before the next one is sent.

use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::driver::{Direction, DriverError, Drivers, os_string};
use crate::ObjectId;

/// The largest payload of one pkt-line.
const MAX_DATA: usize = 65516;

/// A running filter process after a successful handshake.
pub(super) struct Process {
    child: Child,
    input: Option<BufWriter<ChildStdin>>,
    output: Option<BufReader<ChildStdout>>,
    /// Set when the stream went out of step; dropping then kills the filter.
    broken: bool,
    clean: bool,
    smudge: bool,
}

impl std::fmt::Debug for Process {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Process")
            .field("pid", &self.child.id())
            .field("clean", &self.clean)
            .field("smudge", &self.smudge)
            .finish_non_exhaustive()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Closing stdin tells the filter to exit, as Git does. Closing stdout as well means a
        // filter still writing a reply nobody will read gets EPIPE rather than blocking, so the
        // wait cannot hang on a full pipe. After a protocol error the filter may ignore both;
        // it is killed then, since its stream is out of step anyway.
        if let Some(mut input) = self.input.take() {
            let _ = input.flush();
        }
        self.output = None;
        if self.broken {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// The outcome of one request.
pub(super) enum Reply {
    Converted(Vec<u8>),
    /// `status=error`: this file failed; the process remains usable.
    Error,
    /// `status=abort`: the filter refuses further requests of this kind.
    Abort,
}

fn protocol(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned())
}

fn write_packet(writer: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    write!(writer, "{:04x}", data.len() + 4)?;
    writer.write_all(data)
}

fn write_text(writer: &mut impl Write, text: &[u8]) -> std::io::Result<()> {
    let mut line = text.to_vec();
    line.push(b'\n');
    write_packet(writer, &line)
}

fn flush(writer: &mut impl Write) -> std::io::Result<()> {
    writer.write_all(b"0000")
}

/// Reads one packet; `None` is a flush packet.
fn read_packet(reader: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = std::str::from_utf8(&header)
        .ok()
        .and_then(|header| usize::from_str_radix(header, 16).ok())
        .ok_or_else(|| protocol("invalid pkt-line length"))?;
    match length {
        0 => Ok(None),
        1..=4 => Err(protocol("unexpected pkt-line control packet")),
        _ if length > MAX_DATA + 4 => Err(protocol("pkt-line too long")),
        _ => {
            let mut data = vec![0; length - 4];
            reader.read_exact(&mut data)?;
            Ok(Some(data))
        }
    }
}

/// Reads text packets up to a flush, without their trailing LF.
fn read_list(reader: &mut impl Read) -> std::io::Result<Vec<Vec<u8>>> {
    let mut lines = Vec::new();
    while let Some(mut line) = read_packet(reader)? {
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        lines.push(line);
    }
    Ok(lines)
}

/// The last `status=` value of a list.
fn status(list: &[Vec<u8>]) -> Option<&[u8]> {
    list.iter()
        .rev()
        .find_map(|line| line.strip_prefix(b"status="))
}

impl Process {
    /// Starts `command` through the shell and performs the handshake.
    pub(super) fn start(command: &[u8], drivers: &Drivers) -> std::io::Result<Self> {
        let mut shell = Command::new("sh");
        shell.arg("-c").arg(os_string(command.to_vec()));
        drivers.configure_command(&mut shell);
        let mut child = shell
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("filter process pipes unavailable"));
        };
        let mut process = Self {
            child,
            input: Some(BufWriter::new(stdin)),
            output: Some(BufReader::new(stdout)),
            broken: false,
            clean: false,
            smudge: false,
        };
        if let Err(error) = process.handshake() {
            process.broken = true;
            return Err(error);
        }
        Ok(process)
    }

    fn input(&mut self) -> std::io::Result<&mut BufWriter<ChildStdin>> {
        self.input
            .as_mut()
            .ok_or_else(|| std::io::Error::other("filter process input closed"))
    }

    fn output(&mut self) -> std::io::Result<&mut BufReader<ChildStdout>> {
        self.output
            .as_mut()
            .ok_or_else(|| std::io::Error::other("filter process output closed"))
    }

    fn handshake(&mut self) -> std::io::Result<()> {
        let input = self.input()?;
        write_text(input, b"git-filter-client")?;
        write_text(input, b"version=2")?;
        flush(input)?;
        input.flush()?;
        let welcome = read_list(self.output()?)?;
        if welcome.first().map(Vec::as_slice) != Some(b"git-filter-server")
            || !welcome.iter().any(|line| line == b"version=2")
        {
            return Err(protocol("filter process did not accept version 2"));
        }
        let input = self.input()?;
        write_text(input, b"capability=clean")?;
        write_text(input, b"capability=smudge")?;
        flush(input)?;
        input.flush()?;
        for capability in read_list(self.output()?)? {
            match capability.as_slice() {
                b"capability=clean" => self.clean = true,
                b"capability=smudge" => self.smudge = true,
                // Capabilities girt did not offer, such as `delay`, must not be used by the filter.
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn supports(&self, direction: Direction) -> bool {
        match direction {
            Direction::Clean => self.clean,
            Direction::Smudge => self.smudge,
        }
    }

    pub(super) fn disable(&mut self, direction: Direction) {
        match direction {
            Direction::Clean => self.clean = false,
            Direction::Smudge => self.smudge = false,
        }
    }

    /// Sends one file and reads the reply, keeping at most `limit` output bytes.
    pub(super) fn request(
        &mut self,
        direction: Direction,
        path: &[u8],
        blob: Option<ObjectId>,
        content: &[u8],
        limit: usize,
    ) -> Result<Reply, RequestError> {
        let result = self.exchange(direction, path, blob, content, limit);
        if matches!(result, Err(RequestError::Io(_))) {
            self.broken = true;
        }
        result
    }

    fn exchange(
        &mut self,
        direction: Direction,
        path: &[u8],
        blob: Option<ObjectId>,
        content: &[u8],
        limit: usize,
    ) -> Result<Reply, RequestError> {
        let input = self.input()?;
        let mut header = b"command=".to_vec();
        header.extend_from_slice(direction.name().as_bytes());
        write_text(input, &header)?;
        let mut pathname = b"pathname=".to_vec();
        pathname.extend_from_slice(path);
        write_text(input, &pathname)?;
        if let Some(blob) = blob {
            write_text(input, format!("blob={blob}").as_bytes())?;
        }
        flush(input)?;
        for chunk in content.chunks(MAX_DATA) {
            write_packet(input, chunk)?;
        }
        flush(input)?;
        input.flush()?;

        match status(&read_list(self.output()?)?) {
            Some(b"success") => {}
            Some(b"abort") => return Ok(Reply::Abort),
            Some(b"error") => return Ok(Reply::Error),
            _ => return Err(protocol("filter process sent no status").into()),
        }
        let mut output = Vec::new();
        let mut too_large = false;
        while let Some(packet) = read_packet(self.output()?)? {
            if output.len() + packet.len() > limit {
                too_large = true;
            } else {
                output.extend_from_slice(&packet);
            }
        }
        // An empty trailing list keeps the status sent before the content.
        let reply = match status(&read_list(self.output()?)?) {
            None | Some(b"success") => Reply::Converted(output),
            Some(b"abort") => Reply::Abort,
            Some(b"error") => Reply::Error,
            Some(_) => return Err(protocol("unknown filter process status").into()),
        };
        if too_large {
            return Err(RequestError::TooLarge);
        }
        Ok(reply)
    }
}

pub(super) enum RequestError {
    Io(std::io::Error),
    TooLarge,
}

impl From<std::io::Error> for RequestError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl RequestError {
    pub(super) fn into_driver_error(self, name: &[u8], direction: Direction) -> DriverError {
        match self {
            Self::Io(source) => DriverError::Io {
                driver: name.to_vec(),
                direction,
                source,
            },
            Self::TooLarge => DriverError::TooLarge {
                driver: name.to_vec(),
                direction,
            },
        }
    }
}
