//! Original disposable-PTY fixtures. The test runner never opens its own controlling terminal.
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rstest::rstest;
use rustix::process::{getpgrp, getpid, getsid};
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcgetpgrp, tcsetattr};

use super::*;
use crate::transport::ssh::{ForegroundTerminal, TerminalEvents};

const HARNESS: &str = r#"
import os,pty,select,signal,subprocess,sys,time,fcntl,termios
master,slave=pty.openpty()
root=sys.argv[3]
child=os.fork()
if child==0:
    os.setsid()
    signal.pthread_sigmask(signal.SIG_SETMASK,[])
    fcntl.ioctl(slave,termios.TIOCSCTTY,0)
    os.dup2(slave,0)
    os.close(master)
    log=os.open(root+'/log',os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
    os.dup2(log,1); os.dup2(log,2)
    os.environ['GIRT_DISPOSABLE_PTY']=root
    os.environ['GIRT_TERMINAL_CASE']=sys.argv[2]
    os.execv(sys.argv[1],[sys.argv[1],'--exact','transport::ssh::process::terminal_tests::isolated_child','--nocapture','--test-threads=1'])
os.close(slave)
deadline=time.monotonic()+20
transcript=b''
while True:
    ready,_,_=select.select([master],[],[],0.02)
    if ready:
        try: data=os.read(master,8192)
        except OSError: data=b''
        transcript+=data
        if b'prompt:' in transcript:
            os.write(master,b'fixture-secret\n')
            transcript=b''
        if b'partial:' in transcript:
            os.write(master,b'private-tail')
            open(root+'/input-ready','w').write('ready')
            transcript=b''
        if b'typeahead:' in transcript:
            os.write(master,b'next-command')
            open(root+'/input-ready','w').write('ready')
            transcript=b''
        if b'check-input:' in transcript:
            os.write(master,b'\n')
            transcript=b''
    done,status=os.waitpid(child,os.WNOHANG)
    if done:
        print(open(root+'/log').read())
        sys.exit(os.waitstatus_to_exitcode(status))
    if time.monotonic()>deadline:
        os.kill(child,signal.SIGKILL)
        peer=root+'/peer'
        if os.path.exists(peer):
            try: os.killpg(int(open(peer).read()),signal.SIGKILL)
            except ProcessLookupError: pass
        os.waitpid(child,0)
        print(open(root+'/log').read())
        raise RuntimeError('disposable terminal fixture timed out')
"#;

const PEER: &str = r#"
import os,signal,sys,termios,time
root,mode=sys.argv[1:]
open(root+'/peer','w').write(str(os.getpid()))
tty=os.open('/dev/tty',os.O_RDWR)
assert os.tcgetpgrp(tty)==os.getpgrp()
assert signal.SIGTTOU not in signal.pthread_sigmask(signal.SIG_BLOCK,[])
attrs=termios.tcgetattr(tty)
attrs[3]&=~termios.ECHO
termios.tcsetattr(tty,termios.TCSANOW,attrs)
os.write(tty,b'prompt:')
assert os.read(tty,100)==b'fixture-secret\n'
if mode in ('cancel-private','stop-private','exit-private','clean-private','clean-typeahead'):
    if mode=='clean-typeahead':
        attrs[3]|=termios.ECHO
        termios.tcsetattr(tty,termios.TCSANOW,attrs)
        os.write(tty,b'typeahead:')
    else: os.write(tty,b'partial:')
    while not os.path.exists(root+'/input-ready'): time.sleep(0.001)
    if mode=='exit-private': sys.exit(42)
if mode in ('cancel','drop-future','cancel-private'):
    os.write(2,b'ready')
    time.sleep(60)
if mode in ('advertisement','stop-cancel','permit-drop','events-drop','background-resume','resume-failure','rapid-stop','stop-deadline','stop-private'):
    os.kill(os.getpid(),signal.SIGSTOP)
if mode=='rapid-stop': time.sleep(0.2)
os.write(1,b'0000')
if mode=='upload':
    os.read(0,1)
    os.kill(os.getpid(),signal.SIGSTOP)
while os.read(0,8192): pass
if mode=='response':
    os.write(1,b'before')
    os.kill(os.getpid(),signal.SIGSTOP)
    os.write(1,b'after')
if mode=='drain':
    os.close(1)
    os.kill(os.getpid(),signal.SIGSTOP)
if mode=='descendants':
    child=os.fork()
    if child==0: time.sleep(60); sys.exit(0)
    open(root+'/descendant','w').write(str(child))
os.write(2,b'final')
"#;

#[rstest]
#[case::immediate_tty("normal")]
#[case::advertisement_stop("advertisement")]
#[case::upload_stop("upload")]
#[case::response_stop("response")]
#[case::drain_stop("drain")]
#[case::echo_disabled_cancel("cancel")]
#[case::cancel_while_permit_held("stop-cancel")]
#[case::dropped_permit("permit-drop")]
#[case::dropped_events("events-drop")]
#[case::background_resume("background-resume")]
#[case::exec_failure("exec-failure")]
#[case::tostop_refused("tostop")]
#[case::descendants("descendants")]
#[case::sequential_attachment("sequential")]
#[case::resume_failure("resume-failure")]
#[case::rapid_continue_stop("rapid-stop")]
#[case::suspended_deadline("stop-deadline")]
#[case::dropped_future("drop-future")]
#[case::public_shared_attachment("public")]
#[case::cancel_private_input("cancel-private")]
#[case::stop_private_input("stop-private")]
#[case::exit_private_input("exit-private")]
#[case::clean_private_input("clean-private")]
#[case::clean_typeahead("clean-typeahead")]
#[case::failed_flush_returns_ownership("flush-failure")]
fn disposable_terminal(#[case] mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .args(["-c", HARNESS])
        .arg(std::env::current_exe().unwrap())
        .arg(mode)
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// Re-executed only after the Python harness created a new session and disposable controlling PTY.
#[test]
fn isolated_child() {
    let Ok(root) = std::env::var("GIRT_DISPOSABLE_PTY") else {
        return;
    };
    assert_eq!(getsid(None).unwrap(), getpid());
    let mode = std::env::var("GIRT_TERMINAL_CASE").unwrap();
    let fd: OwnedFd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .unwrap()
        .into();
    let before = tcgetattr(&fd).unwrap();
    if mode == "tostop" {
        let mut attrs = before.clone();
        attrs.local_modes.insert(LocalModes::TOSTOP);
        tcsetattr(&fd, OptionalActions::Now, &attrs).unwrap();
        assert!(matches!(
            ForegroundTerminal::prepare(fd),
            Err(SshError::Configuration("terminal TOSTOP is unsupported"))
        ));
        return;
    }
    let (terminal, events) = ForegroundTerminal::prepare(fd.try_clone().unwrap()).unwrap();
    let mut events = Some(events);
    let cancel = AtomicBool::new(false);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut unrelated = Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let supervisor = Pid::from_raw(unrelated.id() as i32).unwrap();
    if mode == "flush-failure" {
        failed_flush_returns_ownership(&root, &terminal, &fd, &before, supervisor);
    } else if mode == "public" {
        let terminal = std::sync::Arc::new(terminal);
        public_roundtrip(&rt, &root, terminal.clone(), &cancel);
        public_roundtrip(&rt, &root, terminal, &cancel);
    } else if mode == "exec-failure" {
        let result = rt.block_on(async {
            Session::spawn_with_terminal(
                &mut Command::new("/not/a/real/executable"),
                Some(&terminal),
            )
        });
        assert!(matches!(result, Err(SshError::Io(_))));
    } else {
        run_peer(
            &rt,
            &root,
            &mode,
            &terminal,
            &mut events,
            &cancel,
            &fd,
            &before,
            supervisor,
        );
        if mode == "sequential" {
            run_peer(
                &rt,
                &root,
                "normal",
                &terminal,
                &mut events,
                &cancel,
                &fd,
                &before,
                supervisor,
            );
        }
    }
    assert_eq!(tcgetpgrp(&fd).unwrap(), getpgrp());
    assert_attributes(&before, &tcgetattr(&fd).unwrap());
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[allow(clippy::too_many_arguments)]
fn run_peer(
    rt: &tokio::runtime::Runtime,
    root: &str,
    mode: &str,
    terminal: &ForegroundTerminal,
    events: &mut Option<TerminalEvents>,
    cancel: &AtomicBool,
    fd: &OwnedFd,
    before: &Termios,
    supervisor: Pid,
) {
    let mut command = Command::new("python3");
    command.args(["-c", PEER, root, mode]);
    let (result, pid, stops) = rt.block_on(async {
        let mut session = Session::spawn_with_terminal(&mut command, Some(terminal)).unwrap();
        assert!(matches!(Session::spawn_with_terminal(&mut Command::new("false"), Some(terminal)), Err(SshError::Configuration("terminal already leased"))));
        let pid = session.process.pid();
        RAPID_STOP.store(mode == "rapid-stop", Ordering::Relaxed);
        let seconds = if mode == "stop-deadline" { 1 } else { 5 };
        let control = TransportControl { cancel, deadline: Some(Instant::now()+Duration::from_secs(seconds)) };
        let drop_ready = AtomicBool::new(false);
        let drop_ready = &drop_ready;
        let mut diagnostics = |bytes: &[u8]| {
            if bytes.windows(5).any(|bytes| bytes == b"ready") {
                if matches!(mode, "cancel" | "cancel-private") { cancel.store(true, Ordering::Relaxed); }
                if mode == "drop-future" { drop_ready.store(true, Ordering::Relaxed); }
            }
        };
        let operation = async move {
            session.advertise_with_diagnostics(4, control, &mut diagnostics).await?;
            let request = vec![b'x'; 1024*1024];
            let (body, result, _) = session.exchange_with_diagnostics(&request, &[], 100, control, &mut diagnostics).await;
            result?;
            if mode == "response" { assert_eq!(body, b"beforeafter"); }
            Ok::<_, SshError>(())
        };
        tokio::pin!(operation);
        let mut stops = 0;
        let mut held = None;
        let result = loop {
            tokio::select! {
                result = &mut operation => break result,
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    if drop_ready.load(Ordering::Relaxed) { break Err(SshError::Cancelled); }
                    let permit = events.as_mut().and_then(|events| events.try_next_stop().ok().flatten());
                    if let Some(permit) = permit {
                        stops += 1;
                        assert_eq!(permit.signal(), libc::SIGSTOP);
                        assert_eq!(tcgetpgrp(fd).unwrap(), getpgrp());
                        assert_attributes(before, &tcgetattr(fd).unwrap());
                        match mode {
                            "stop-cancel" => { held = Some(permit); cancel.store(true, Ordering::Relaxed); }
                            "stop-deadline" => { held = Some(permit); }
                            "permit-drop" => drop(permit),
                            "events-drop" => { held = Some(permit); events.take(); }
                            "background-resume" => {
                                super::super::terminal::with_sigttou_blocked(|| rustix::termios::tcsetpgrp(fd, supervisor)).unwrap();
                                assert!(matches!(permit.resume(), Err(SshError::Configuration(_))));
                            }
                            "resume-failure" => {
                                permit.resume().unwrap();
                                super::super::terminal::FAIL_RESUME.store(true, Ordering::Relaxed);
                            }
                            _ => permit.resume().unwrap(),
                        }
                    }
                }
            }
        };
        // A retained permit cannot delay cancellation cleanup or restore the terminal later.
        drop(held);
        (result, pid, stops)
    });
    match mode {
        "cancel" | "cancel-private" | "drop-future" | "stop-cancel" | "permit-drop"
        | "events-drop" | "background-resume" => {
            assert!(matches!(result, Err(SshError::Cancelled)), "{result:?}")
        }
        "exit-private" => assert!(
            matches!(result, Err(SshError::Exit(Some(42)))),
            "{result:?}"
        ),
        "stop-deadline" => assert!(matches!(result, Err(SshError::Deadline)), "{result:?}"),
        "resume-failure" => assert!(matches!(result, Err(SshError::Io(_))), "{result:?}"),
        _ => result.unwrap(),
    }
    let expected = if mode == "rapid-stop" {
        2
    } else {
        usize::from(matches!(
            mode,
            "advertisement"
                | "upload"
                | "response"
                | "drain"
                | "stop-cancel"
                | "permit-drop"
                | "events-drop"
                | "background-resume"
                | "resume-failure"
                | "stop-deadline"
                | "stop-private"
        ))
    };
    assert_eq!(stops, expected);
    assert!(matches!(
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG
        ),
        Err(rustix::io::Errno::CHILD)
    ));
    if mode == "background-resume" {
        assert_eq!(tcgetpgrp(fd).unwrap(), supervisor);
        super::super::terminal::with_sigttou_blocked(|| rustix::termios::tcsetpgrp(fd, getpgrp()))
            .unwrap();
    }
    assert_eq!(tcgetpgrp(fd).unwrap(), getpgrp());
    assert_attributes(before, &tcgetattr(fd).unwrap());
    if mode == "descendants" {
        assert_descendant_dead(root);
    }
    if matches!(
        mode,
        "cancel-private" | "stop-private" | "exit-private" | "clean-private" | "clean-typeahead"
    ) {
        let mut tty = std::fs::File::from(fd.try_clone().unwrap());
        tty.write_all(b"check-input:").unwrap();
        let mut input = [0; 100];
        let count = tty.read(&mut input).unwrap();
        let expected: &[u8] = if mode == "clean-typeahead" {
            b"next-command\n"
        } else {
            b"\n"
        };
        assert_eq!(&input[..count], expected);
    }
}

fn assert_attributes(before: &Termios, after: &Termios) {
    assert_eq!(before.input_modes, after.input_modes);
    assert_eq!(before.output_modes, after.output_modes);
    assert_eq!(before.control_modes, after.control_modes);
    assert_eq!(before.local_modes, after.local_modes);
    // SpecialCodes exposes a complete Debug representation but no equality operation.
    assert_eq!(
        format!("{:?}", before.special_codes),
        format!("{:?}", after.special_codes)
    );
    assert_eq!(before.input_speed(), after.input_speed());
    assert_eq!(before.output_speed(), after.output_speed());
}

fn assert_descendant_dead(root: &str) {
    let pid: i32 = std::fs::read_to_string(format!("{root}/descendant"))
        .unwrap()
        .parse()
        .unwrap();
    let pid = Pid::from_raw(pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while rustix::process::test_kill_process(pid).is_ok() {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.as_raw_nonzero().to_string()])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&output.stdout)
            .trim_start()
            .starts_with('Z')
        {
            return;
        }
        assert!(Instant::now() < deadline, "owned descendant remains alive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Force a CONT-to-STOP transition between peek and consume to exercise notification replacement.
pub(super) static RAPID_STOP: AtomicBool = AtomicBool::new(false);

pub(super) fn replace_continued(pid: Pid) {
    kill_process_group(pid, Signal::STOP).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while waitid(
        WaitId::Pid(pid),
        WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .unwrap()
    .is_none()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn public_roundtrip(
    rt: &tokio::runtime::Runtime,
    root: &str,
    terminal: std::sync::Arc<ForegroundTerminal>,
    cancel: &AtomicBool,
) {
    use std::os::unix::fs::PermissionsExt;

    use crate::remote::{ProtocolEnvironment, Remote};
    use crate::transport::ssh::{OpenSshOptions, SshRemote};
    let executable = std::path::Path::new(root).join("ssh");
    let script = format!(
        "#!/usr/bin/env python3\n{}",
        PEER.replace(
            "root,mode=sys.argv[1:]",
            "root,mode=os.environ['FIXTURE_ROOT'],'normal'"
        )
    );
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = crate::Config::parse(b"[remote \"r\"]\nurl = host:repo\n").unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let remote = SshRemote::openssh(
        &config,
        &destination,
        OpenSshOptions {
            default_executable: executable,
            environment: [
                ("PATH".into(), std::env::var_os("PATH").unwrap()),
                ("FIXTURE_ROOT".into(), root.into()),
            ]
            .into(),
            ..Default::default()
        },
    )
    .unwrap()
    .with_terminal(terminal)
    .unwrap();
    assert!(remote.has_terminal());
    fn assert_send<T: Send>(_: &T) {}
    let operation = crate::fetch::discover_ssh(
        &remote,
        crate::fetch::FetchLimits::default(),
        TransportControl {
            cancel,
            deadline: Some(Instant::now() + Duration::from_secs(5)),
        },
    );
    assert_send(&operation);
    let found = rt.block_on(operation).unwrap();
    assert!(found.advertisement.refs.is_empty());
}

fn failed_flush_returns_ownership(
    root: &str,
    terminal: &ForegroundTerminal,
    fd: &OwnedFd,
    before: &Termios,
    supervisor: Pid,
) {
    use super::super::terminal::{FAIL_FLUSH, with_sigttou_blocked};
    let mut lease = terminal.acquire().unwrap();
    let mut private = before.clone();
    private.local_modes.remove(LocalModes::ECHO);
    with_sigttou_blocked(|| {
        tcsetattr(fd, OptionalActions::Now, &private)?;
        rustix::termios::tcsetpgrp(fd, supervisor)
    })
    .unwrap();
    FAIL_FLUSH.store(true, Ordering::Relaxed);
    assert!(matches!(lease.restore(false), Err(SshError::Io(_))));
    assert_eq!(tcgetpgrp(fd).unwrap(), getpgrp());
    assert_attributes(before, &tcgetattr(fd).unwrap());
    let mut tty = std::fs::File::from(fd.try_clone().unwrap());
    tty.write_all(b"typeahead:").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !std::path::Path::new(root).join("input-ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(lease);
    tty.write_all(b"check-input:").unwrap();
    let mut input = [0; 100];
    let count = tty.read(&mut input).unwrap();
    assert_eq!(&input[..count], b"next-command\n");
}
