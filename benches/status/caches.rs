//! Original Git-generated index caches; fixture setup is outside each timed operation.
#[path = "../../tests/support/layout_git.rs"]
mod oracle;

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion};
use girt::status::{Baseline, Scan, UntrackedFiles};
use girt::{InitKind, ObjectFormat, Repository};

pub(super) fn benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("configured_status_caches");
    for count in [1_000, 10_000] {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            temporary.path().join("r"),
            InitKind::Worktree,
        )
        .unwrap();
        let root = repo.worktree().unwrap();
        for directory in 0..count / 100 {
            let path = root.join(format!("dir-{directory:04}"));
            std::fs::create_dir(&path).unwrap();
            for file in 0..100 {
                std::fs::write(path.join(format!("file-{file:04}")), b"data\n").unwrap();
            }
        }
        for (key, value) in [
            ("core.ignoreCase", "false"),
            ("core.precomposeUnicode", "false"),
            ("core.excludesFile", "/girt-absent-excludes"),
            ("core.fsmonitor", "printf 'benchmark\\000'; :"),
            ("core.fsmonitorHookVersion", "2"),
            ("core.untrackedCache", "true"),
        ] {
            oracle::git(root, &["config", key, value], b"");
        }
        oracle::git(root, &["add", "."], b"");
        oracle::git(
            root,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"],
            b"",
        );
        oracle::git(root, &["status", "--porcelain"], b"");
        oracle::git(root, &["status", "--porcelain"], b"");
        std::fs::File::options()
            .write(true)
            .open(repo.git_dir().join("index"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
            .unwrap();
        let cached = Repository::open(root).unwrap();
        oracle::git(root, &["config", "core.untrackedCache", "false"], b"");
        let ordinary = Repository::open(root).unwrap();
        let environment = girt::workflow::Environment::default();
        let rules = cached.ignore_rules(&environment).unwrap();
        let monitor = cached
            .fsmonitor(&environment, girt::fsmonitor::Commands::Run)
            .unwrap()
            .unwrap();
        for (name, repository, monitored) in [
            ("ordinary", &ordinary, false),
            ("untracked", &cached, false),
            ("fsmonitor", &ordinary, true),
            ("both", &cached, true),
        ] {
            let run = || {
                let mut scan = Scan::new(
                    repository,
                    Baseline::Head,
                    UntrackedFiles::Collapsed,
                    &rules,
                );
                if monitored {
                    scan = scan.with_monitor(&monitor);
                }
                scan.run().unwrap()
            };
            let report = run();
            assert!(report.unstaged.is_empty());
            assert!(report.untracked.is_empty());
            group.bench_function(BenchmarkId::new(name, count), |b| {
                b.iter(|| black_box(run()))
            });
        }
    }
    group.finish();
}
