use std::ops::ControlFlow;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rstest::rstest;

use super::receive_server;
use crate::fetch::{FetchError, FetchLimits};
use crate::transport::TransportControl;

#[rstest]
#[case::advertisement("sleep 3")]
#[case::after_advertisement("printf 0000; read request; sleep 3")]
#[case::exit_wait("printf 0000; dd bs=1 count=4 of=/dev/null 2>/dev/null; exec 1>&-; sleep 3")]
fn deadline_interrupts_fetch_phases(#[case] script: &str) {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "(sleep 8; kill -KILL -$$) </dev/null >/dev/null 2>&1 &\n{script}"
    ));
    let cancel = AtomicBool::new(false);
    let result = receive_server(
        &mut command,
        |_| vec![],
        FetchLimits::default(),
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_millis(500)),
        },
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Deadline)));
}
