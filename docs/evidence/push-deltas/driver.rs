use std::collections::BTreeSet;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use girt::fetch::{FetchLimits, KnownHistory};
use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
use girt::transfer::{Environment, FetchOptions, NoCallbacks, PushOptions};
use girt::push::{ForcePolicy, PushCommand};
use girt::{HistoryLimits, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Signature, TreeCompareLimits};

struct Disposable;
impl MaintenanceIsolation for Disposable {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> { Ok(()) }
}

fn phase(label: &str, start: Instant) -> Instant {
    eprintln!("phase {label}={:.6}", start.elapsed().as_secs_f64());
    Instant::now()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let op = args[1].as_str();
    let path = &args[2];
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    match op {
        // Startup: open repository, resolve HEAD, read the commit.
        "open-head" => {
            let repo = Repository::open(path)?;
            let t = phase("open", t);
            let refs = repo.references()?;
            let head = refs.resolve(&RefName::HEAD, 10)?;
            let t = phase("resolve", t);
            let objects = repo.objects(PackLimits::trusted())?;
            let t = phase("objects", t);
            let id = head.id.unwrap();
            let obj = objects.read(id, ReadLimits::trusted())?.unwrap();
            phase("read", t);
            println!("{} {}", id, obj.data().len());
        }
        // Ref enumeration.
        "refs-list" => {
            let repo = Repository::open(path)?;
            let refs = repo.references()?;
            let list = refs.list()?;
            println!("{}", list.len());
        }
        // Commit-only history walk from all refs.
        "walk-commits" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let roots = direct_tips(&repo)?;
            let ids = objects.walk(&roots, HistoryLimits::trusted())?;
            println!("{}", ids.len());
        }
        // Every reachable object decoded (commit, tree, blob), like `rev-list --objects` + cat-file.
        "walk-objects" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let roots = direct_tips(&repo)?;
            let commits = objects.walk(&roots, HistoryLimits::trusted())?;
            let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
            let mut bytes = 0usize;
            let mut stack = Vec::new();
            for c in &commits {
                let obj = objects.read(*c, ReadLimits::trusted())?.unwrap();
                bytes += obj.data().len();
                let commit = girt::Commit::parse(repo.object_format(), obj.data())?;
                if seen.insert(commit.tree()) { stack.push(commit.tree()); }
            }
            while let Some(id) = stack.pop() {
                let obj = objects.read(id, ReadLimits::trusted())?.unwrap();
                bytes += obj.data().len();
                if obj.kind() == ObjectKind::Tree {
                    let tree = girt::Tree::parse(repo.object_format(), obj.data())?;
                    for e in tree.entries() {
                        if e.mode != girt::EntryMode::Gitlink && seen.insert(e.id) { stack.push(e.id); }
                    }
                }
            }
            println!("{} {}", seen.len() + commits.len(), bytes);
        }
        // Tree diff between two commits.
        "diff" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let a = tree_of(&repo, &objects, &args[3])?;
            let b = tree_of(&repo, &objects, &args[4])?;
            let changes = objects.compare_trees(Some(a), Some(b), TreeCompareLimits { max_trees: usize::MAX, max_tree_bytes: usize::MAX, max_entries: usize::MAX, max_path_bytes: usize::MAX, max_changes: usize::MAX, ..Default::default() }, &cancel)?;
            println!("{}", changes.len());
        }
        // Many consecutive tree diffs (jj's pattern): each commit against its first parent.
        "diff-many" => {
            let n: usize = args[3].parse()?;
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let head = head_id(&repo)?;
            let mut id = head;
            let mut total = 0usize;
            for _ in 0..n {
                let commit = girt::Commit::parse(repo.object_format(), objects.read(id, ReadLimits::trusted())?.unwrap().data())?;
                let Some(parent) = commit.parents().first().copied() else { break };
                let pc = girt::Commit::parse(repo.object_format(), objects.read(parent, ReadLimits::trusted())?.unwrap().data())?;
                total += objects.compare_trees(Some(pc.tree()), Some(commit.tree()), TreeCompareLimits { max_trees: usize::MAX, max_tree_bytes: usize::MAX, max_entries: usize::MAX, max_path_bytes: usize::MAX, max_changes: usize::MAX, ..Default::default() }, &cancel)?.len();
                id = parent;
            }
            println!("{total}");
        }
        // Write N loose blobs.
        "write-blobs" => {
            let n: usize = args[3].parse()?;
            let repo = Repository::open(path)?;
            let loose = repo.loose_objects();
            let mut last = None;
            for i in 0..n {
                let data = format!("blob {i} {}\n", "x".repeat(i % 4000));
                last = Some(loose.write_blob(data.as_bytes())?);
            }
            println!("{last:?}");
        }
        // Update N refs in one transaction, with reflog.
        "refs-update" => {
            let n: usize = args[3].parse()?;
            let repo = Repository::open(path)?;
            let head = head_id(&repo)?;
            let sig = Signature { name: b"bench".to_vec(), email: b"bench@example.com".to_vec(), seconds: 1_700_000_000, offset_minutes: 0 };
            let edits: Vec<RefEdit> = (0..n).map(|i| RefEdit {
                name: RefName::new(&format!("refs/jj/keep/{i:06}")).unwrap(),
                dereference: false,
                target: Some(Target::Direct(head)),
                expected: Expected::Any,
                reflog: Reflog::Append { committer: sig.clone(), message: b"bench".to_vec() },
            }).collect();
            let refs = repo.references()?;
            let out = refs.transaction(&edits)?;
            println!("{}", out.len());
        }
        // N single-ref transactions, one at a time (jj's export pattern if it does that).
        "refs-update-each" => {
            let n: usize = args[3].parse()?;
            let repo = Repository::open(path)?;
            let head = head_id(&repo)?;
            let refs = repo.references()?;
            for i in 0..n {
                refs.transaction(&[RefEdit {
                    name: RefName::new(&format!("refs/jj/keep/{i:06}"))?,
                    dereference: false,
                    target: Some(Target::Direct(head)),
                    expected: Expected::Any,
                    reflog: Reflog::Preserve,
                }])?;
            }
            println!("{n}");
        }
        // Configured-remote fetch (origin), default refspec from config.
        "fetch" => {
            let repo = Repository::open(path)?;
            let env = Environment::from_process();
            let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
            let out = repo.fetch("origin", &options, &env, &mut NoCallbacks)?;
            println!("updated={} missing={}", out.updated.len(), out.missing_sources.len());
        }
        // KnownHistory alone (the no-op fetch's local-side cost).
        "known-history" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let tips = direct_tips(&repo)?;
            let known = KnownHistory::from_store(objects, &tips, FetchLimits::trusted(), &cancel)?;
            let _ = known;
            println!("ok");
        }
        // Configured-remote push of one branch.
        "prepare-push" => {
            let repo = Repository::open(path)?;
            let t = phase("repo-open", t);
            let objects = repo.objects(PackLimits::trusted())?;
            let t = phase("store-open", t);
            let new: ObjectId = args[3].parse()?;
            let old: ObjectId = args[4].parse()?;
            let mut t = t;
            for _ in 0..args.get(5).and_then(|x| x.parse().ok()).unwrap_or(1) {
            let prepared = girt::push::PreparedPush::new_local_with_progress(
                &objects,
                vec![PushCommand { name: RefName::new("refs/heads/bench-push")?, expected: Some(old), new, force: ForcePolicy::FastForwardOnly }],
                &[old], girt::push::PushLimits::default(), &cancel,
                |e| {
                    if matches!(e, girt::push::PreparationProgress::Packing { objects: (0, _) }) { t = phase("select", t); }
                    if e == girt::push::PreparationProgress::Complete { phase("write", t); }
                },
            )?;
            println!("objects={} bytes={}", prepared.object_count(), prepared.pack_bytes());
            }
        }
        "push" => {
            let repo = Repository::open(path)?;
            let env = Environment::from_process();
            let id: ObjectId = args[3].parse()?;
            let expected: Option<ObjectId> = args.get(4).filter(|s| s.as_str() != "-").map(|s| s.parse()).transpose()?;
            let commands = vec![PushCommand { name: RefName::new("refs/heads/bench-push")?, expected, new: id, force: ForcePolicy::FastForwardOnly }];
            let report = repo.push("origin", commands, &PushOptions::default(), &env, &mut NoCallbacks)?;
            println!("{} {:?}", report.all_succeeded(), report.refs);
        }
        "repack" => {
            let repo = Repository::open(path)?;
            let policy = RetentionPolicy { recent_cutoff: SystemTime::now() + Duration::from_secs(60), ..RetentionPolicy::trusted() };
            let t = phase("open", t);
            let plan = repo.plan_retention(&policy, &cancel);
            let t = phase("plan", t);
            eprintln!("required={} complete={}", plan.required.len(), plan.is_complete());
            drop(plan);
            let result = repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)?;
            phase("repack", t);
            println!("{:?}", result.written);
        }
        "maintenance" => {
            let repo = Repository::open(path)?;
            let policy = RetentionPolicy { recent_cutoff: SystemTime::now() + Duration::from_secs(60), ..RetentionPolicy::trusted() };
            let result = repo.run_maintenance(&mut Disposable, &policy, RepackLimits::trusted(), &cancel)?;
            println!("{result:?}");
        }
        // jj's `util gc` without repack: expire reflogs then prune loose.
        "gc-jj" => {
            let repo = Repository::open(path)?;
            let policy = RetentionPolicy::trusted();
            let r1 = repo.expire_reflogs(&mut Disposable, &policy, &cancel)?;
            let t = phase("expire", t);
            let r2 = repo.prune_unreachable_loose(&mut Disposable, &policy, &cancel)?;
            phase("prune", t);
            println!("{r1:?} {r2:?}");
        }
        "index-read" => {
            let repo = Repository::open(path)?;
            let index = repo.read_index(girt::index::Limits::trusted())?.unwrap();
            println!("{}", index.entries().len());
        }
        "status" => {
            let repo = Repository::open(path)?;
            let limits = girt::status::Limits { index: girt::index::Limits::trusted(), trees: TreeCompareLimits { max_trees: usize::MAX, max_tree_bytes: usize::MAX, max_entries: usize::MAX, max_path_bytes: usize::MAX, max_changes: usize::MAX, ..Default::default() }, packs: PackLimits::trusted(), read: ReadLimits::trusted(), ..Default::default() };
            let report = repo.raw_status(girt::status::Baseline::Head, girt::status::Untracked::RawFilesWithoutIgnores, limits, &cancel)?;
            println!("staged={} unstaged={} untracked={}", report.staged.len(), report.unstaged.len(), report.raw_untracked.len());
        }

        // jj's per-command pattern: open + pack snapshot, repeated N times.
        "open-repeat" => {
            let n: usize = args[3].parse()?;
            let mut total = 0usize;
            for _ in 0..n {
                let repo = Repository::open(path)?;
                let objects = repo.objects(PackLimits::trusted())?;
                total += objects.store_directories().count();
            }
            println!("{total}");
        }
        // Objects::refresh (pack rescan jj runs after every fetch).
        "refresh" => {
            let repo = Repository::open(path)?;
            let mut objects = repo.objects(PackLimits::trusted())?;
            let t = phase("open+objects", t);
            objects.refresh(PackLimits::trusted(), girt::AlternateLimits::default())?;
            phase("refresh", t);
            println!("ok");
        }
        // Import pattern: list namespaces, peel every ref.
        "peel-all" => {
            let repo = Repository::open(path)?;
            let refs = repo.references()?;
            let objects = repo.objects(PackLimits::trusted())?;
            let mut n = 0usize;
            for ns in ["refs/heads", "refs/remotes", "refs/tags"] {
                for obs in refs.list_namespace_observations(&RefName::new(ns)?)? {
                    if let Target::Direct(id) = obs.target {
                        let _ = objects.peel(id, girt::PeelLimits::trusted(), &cancel)?;
                        n += 1;
                    }
                }
            }
            println!("{n}");
        }
        // reset_index pattern: enumerate the HEAD tree, rebuild the whole index, commit it.
        "index-write" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let head = head_id(&repo)?;
            let tree = tree_of(&repo, &objects, &head.to_string())?;
            let changes = objects.compare_trees(None, Some(tree), TreeCompareLimits { max_trees: usize::MAX, max_tree_bytes: usize::MAX, max_entries: usize::MAX, max_path_bytes: usize::MAX, max_changes: usize::MAX, ..Default::default() }, &cancel)?;
            let t = phase("tree-walk", t);
            let mut entries = Vec::with_capacity(changes.len());
            for c in changes {
                let v = c.new.unwrap();
                let mode = match v.mode { girt::EntryMode::Executable => girt::index::Mode::Executable, girt::EntryMode::Symlink => girt::index::Mode::Symlink, girt::EntryMode::Gitlink => girt::index::Mode::Gitlink, _ => girt::index::Mode::Regular };
                entries.push(girt::index::Entry::new(c.path, mode, v.id));
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            let n = entries.len();
            let mut edit = repo.edit_index(girt::index::Limits::trusted())?;
            edit.replace_entries(entries)?;
            edit.commit()?;
            phase("index-commit", t);
            println!("{n}");
        }
        // Ignore matching: root .gitignore rules against every path on stdin (NUL-separated).
        "ignore-check" => {
            let content = std::fs::read(&args[3])?;
            let mut rules = girt::ignore::Ignore::new(girt::ignore::Case::Sensitive, girt::ignore::Limits::trusted());
            rules.add(girt::ignore::Source::Directory(b""), &content)?;
            let mut input = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)?;
            let mut ignored = 0usize;
            for p in input.split(|b| *b == b'\n').filter(|p| !p.is_empty()) {
                if rules.check(p, false)?.is_some_and(|m| m.ignored) { ignored += 1; }
            }
            println!("{ignored}");
        }
        // jj's write_commit pattern: loose commit + one keep-ref transaction each.
        "commit-loop" => {
            let n: usize = args[3].parse()?;
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let head = head_id(&repo)?;
            let hc = girt::Commit::parse(repo.object_format(), objects.read(head, ReadLimits::trusted())?.unwrap().data())?;
            let loose = repo.loose_objects();
            let refs = repo.references()?;
            let sig = Signature { name: b"bench".to_vec(), email: b"bench@example.com".to_vec(), seconds: 1_700_000_000, offset_minutes: 0 };
            let mut parent = head;
            for i in 0..n {
                let c = girt::Commit::new(girt::CommitFields { tree: hc.tree(), parents: vec![parent], author: sig.clone(), committer: sig.clone(), extra_headers: vec![], message: format!("bench {i}\n").into_bytes() })?;
                parent = loose.write_commit(&c)?;
                refs.transaction(&[RefEdit { name: RefName::new(&format!("refs/jj/keep/{parent}"))?, dereference: false, target: Some(Target::Direct(parent)), expected: Expected::AbsentOr(Target::Direct(parent)), reflog: Reflog::Preserve }])?;
            }
            println!("{parent}");
        }

        // Local clone into a new bare repository: path = source, args[3] = destination.
        "clone" => {
            use std::ops::ControlFlow;
            let source = std::fs::canonicalize(path)?;
            let dest = std::path::PathBuf::from(&args[3]);
            let request = girt::clone::CloneRequest::prepare_tracking(&dest, girt::InitKind::Bare, source.to_str().unwrap().as_bytes(), girt::clone::BranchSelection::Default, Reflog::Preserve)?;
            let ready = request.receive_local(&source, FetchLimits::trusted(), girt::transport::TransportControl::new(&cancel), |_| ControlFlow::Continue(()))?;
            let t = phase("receive", t);
            let report = ready.finish(girt::fetch::FetchUpdateLimits::trusted(), &cancel).map_err(|e| e.source)?;
            phase("finish", t);
            println!("{}", report.repository.is_some());
        }
        _ => return Err(format!("unknown op {op}").into()),
    }
    let _ = std::io::stdout().flush();
    Ok(())
}

fn direct_tips(repo: &Repository) -> Result<Vec<ObjectId>, Box<dyn std::error::Error>> {
    let refs = repo.references()?;
    let mut tips: BTreeSet<ObjectId> = refs.list()?.into_iter().filter_map(|r| match r.target { Target::Direct(id) => Some(id), _ => None }).collect();
    tips.insert(head_id(repo)?);
    // Peel tags to commits, as rev-list does.
    let objects = repo.objects(PackLimits::trusted())?;
    let cancel = AtomicBool::new(false);
    let mut commits = BTreeSet::new();
    for id in tips {
        let peeled = objects.peel(id, girt::PeelLimits::trusted(), &cancel)?;
        if peeled.kind == ObjectKind::Commit { commits.insert(peeled.target); }
    }
    Ok(commits.into_iter().collect())
}

fn head_id(repo: &Repository) -> Result<ObjectId, Box<dyn std::error::Error>> {
    Ok(repo.references()?.resolve(&RefName::HEAD, 10)?.id.ok_or("unborn HEAD")?)
}

fn tree_of(repo: &Repository, objects: &girt::Objects, spec: &str) -> Result<ObjectId, Box<dyn std::error::Error>> {
    let id: ObjectId = spec.parse()?;
    let obj = objects.read(id, ReadLimits::trusted())?.ok_or("missing")?;
    match obj.kind() {
        ObjectKind::Commit => Ok(girt::Commit::parse(repo.object_format(), obj.data())?.tree()),
        ObjectKind::Tree => Ok(id),
        k => Err(format!("{k:?}").into()),
    }
}
