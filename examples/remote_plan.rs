//! Run `cargo run --example remote_plan`; only a disposable local fixture is written.
//! No discovery session, transfer, ref update or network operation is executed.
use std::io::Write;
use std::sync::atomic::AtomicBool;

use girt::fetch::{AdvertisedRef, Advertisement};
use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits};
use girt::refs::RefName;
use girt::remote::{Mapping, RefSource, Remote};
use girt::{InitKind, ObjectId, PackLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(directory.path().join("repo"), InitKind::Bare)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(repo.common_dir().join("config"))?;
    file.write_all(b"\n[remote \"origin\"]\nurl=https://example.invalid/repo\nfetch=refs/tags/*:refs/tags/*\npush=refs/tags/local:refs/tags/published\n")?;
    // Configuration is a snapshot: reopen after fixture setup.
    let repo = Repository::open(repo.git_dir())?;
    let remote = Remote::find(repo.config(), b"origin")?.ok_or("missing origin")?;
    assert_eq!(
        remote.fetch_url(),
        Some(b"https://example.invalid/repo".as_slice())
    );

    // Supply the Advertisement received by an existing fetch adapter's selection callback.
    // This example uses a synthetic advertisement instead of connecting to a server.
    let tag_name = RefName::new("refs/tags/remote")?;
    let advertised_id = ObjectId::for_blob(b"advertised fixture");
    let advertisement = Advertisement {
        refs: vec![AdvertisedRef {
            name: tag_name.clone(),
            id: advertised_id,
            peeled: false,
        }],
        capabilities: Vec::new(),
    };
    let mut planned = None;
    let mut select = |advertisement: &Advertisement| {
        let result = remote.fetch_refspecs().map_advertisement(advertisement);
        let ids = result
            .as_ref()
            .map_or_else(|_| Vec::new(), |plan| selected_ids(plan));
        planned = Some(result);
        ids
    };
    // Pass `select` to receive_local/receive_http/receive_ssh in a real transfer. Here we call it
    // directly, then check its saved error before accepting any transfer outcome.
    let wants = select(&advertisement);
    let fetch_plan = planned.ok_or("selection callback was not invoked")??;
    assert_eq!(wants, vec![advertised_id]);
    assert_eq!(fetch_plan[0].destination, Some(tag_name));
    // Current fetch callbacks return IDs only, so mapping errors travel through the saved result.
    // Retain the plan for a separate conditional ref transaction after validated installation.

    let local_id = repo.loose_objects()?.write_blob(b"local tag target")?;
    let local_sources = [RefSource {
        name: RefName::new("refs/tags/local")?,
        id: local_id,
    }];
    let push_plan = remote.push_refspecs().map(&local_sources)?;
    let commands = creation_commands(&push_plan)?;
    assert_eq!(commands[0].name.as_bytes(), b"refs/tags/published");
    let objects = repo.objects(PackLimits::default())?;
    let cancel = AtomicBool::new(false);
    let _prepared = PreparedPush::new(&objects, commands, PushLimits::default(), &cancel)?;
    // Sending would be a separate, authorized call to push::send_local/send_http/send_ssh.
    // Select and validate the endpoint from remote.push_urls() before choosing an adapter.
    println!(
        "Planned {} fetch selection and {} conditional push creation; sent nothing",
        wants.len(),
        push_plan.len()
    );
    Ok(())
}

fn selected_ids(plan: &[Mapping]) -> Vec<ObjectId> {
    let mut ids: Vec<_> = plan
        .iter()
        .filter_map(|mapping| mapping.source.as_ref())
        .map(|source| source.id)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

// Example consumer policy: create destinations only, reject configured force intent and deletion.
// A consumer replacing refs must independently supply exact observed old values and authorize
// force.
fn creation_commands(plan: &[Mapping]) -> Result<Vec<PushCommand>, Box<dyn std::error::Error>> {
    let mut commands = Vec::new();
    for mapping in plan {
        if mapping.force {
            return Err("this consumer has not authorized force".into());
        }
        let source = mapping
            .source
            .as_ref()
            .ok_or("push transport does not support deletion")?;
        let destination = mapping
            .destination
            .as_ref()
            .ok_or("push requires a destination")?;
        commands.push(PushCommand {
            name: destination.clone(),
            expected: None,
            new: source.id,
            force: ForcePolicy::FastForwardOnly,
        });
    }
    Ok(commands)
}
