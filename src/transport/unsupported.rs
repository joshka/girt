use std::io;
use std::process::{Command, ExitStatus};

use super::TransportControl;

pub(crate) struct Server;
impl Server {
    pub(crate) fn spawn(_: &mut Command, control: TransportControl<'_>) -> io::Result<Self> {
        control.check()?;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned transports require macOS or Linux",
        ))
    }
    pub(crate) fn streams(&mut self) -> (io::Empty, io::Sink) {
        unreachable!()
    }
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        unreachable!()
    }
}
