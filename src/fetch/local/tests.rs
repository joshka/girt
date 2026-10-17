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

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn native_wants_match_git_upload_pack_in_selection_order(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    git(&[
        "init",
        "--bare",
        "--template=",
        "--initial-branch=main",
        &format!("--object-format={format}"),
        ".",
    ]);
    let parse = |bytes: Vec<u8>| -> crate::ObjectId {
        std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap()
    };
    let tree = git(&["mktree"]);
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let one = parse(git(&[
        "-c",
        "user.name=Fetch Test",
        "-c",
        "user.email=fetch@example.invalid",
        "commit-tree",
        tree,
        "-m",
        "one",
    ]));
    let two = parse(git(&[
        "-c",
        "user.name=Fetch Test",
        "-c",
        "user.email=fetch@example.invalid",
        "commit-tree",
        tree,
        "-m",
        "two",
    ]));
    git(&["update-ref", "refs/heads/main", &one.to_string()]);
    git(&["update-ref", "refs/heads/two", &two.to_string()]);
    let cancel = AtomicBool::new(false);
    for (selection, expected) in [
        (vec![one, one], vec![one]),
        (vec![two, one, two, one], vec![two, one]),
    ] {
        for max_wants in [selection.len(), selection.len() - 1] {
            let limits = FetchLimits::default().with_max_wants(max_wants);
            let native = crate::fetch::receive_local(
                root.path(),
                |_| selection.clone(),
                crate::fetch::LocalFetchRequest::new(limits),
                TransportControl::new(&cancel),
                crate::fetch::LocalFetchObserver::default(),
            );
            let mut command = Command::new("git");
            command.arg("upload-pack").arg(root.path());
            let wire = receive_server(
                &mut command,
                |_| selection.clone(),
                limits,
                TransportControl::new(&cancel),
                |_| ControlFlow::Continue(()),
            );
            if max_wants < selection.len() {
                assert!(matches!(native, Err(FetchError::Limit("wants"))));
                assert!(matches!(wire, Err(FetchError::Limit("wants"))));
            } else {
                assert_eq!(wire.unwrap().wants(), expected);
                assert_eq!(native.unwrap().wants(), expected);
            }
        }
    }
    let source = crate::Repository::open(root.path()).unwrap();
    for (selection, accepted) in [(vec![one, one], true), (vec![one, two], false)] {
        let result = crate::fetch::local_native::receive_depth(
            &source,
            |_| selection,
            &crate::fetch::KnownHistory::default(),
            std::num::NonZeroU32::new(1),
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |_| ControlFlow::Continue(()),
            |_| {},
        );
        if accepted {
            assert_eq!(result.unwrap().wants(), [one]);
        } else {
            assert!(matches!(
                result,
                Err(FetchError::Unsupported("native local depth selection"))
            ));
        }
    }
}
