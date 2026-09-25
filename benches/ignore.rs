//! Matched original batches: root literal and wildcard rules, positive and negative results.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::ignore::{Case, Ignore, Limits, Source};

struct Fixture {
    root: tempfile::TempDir,
    rules: Ignore,
    paths: Vec<Vec<u8>>,
    input: Vec<u8>,
}
impl Fixture {
    fn new(patterns: usize, format: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "-q", &format!("--object-format={format}")])
            .arg(root.path())
            .status()
            .unwrap();
        assert!(status.success());
        let mut content = Vec::new();
        for n in 0..patterns {
            writeln!(content, "generated-{n}.tmp\nmodule-{n}/**/*.cache").unwrap();
        }
        content.extend_from_slice(b"!keep.tmp\n");
        std::fs::write(root.path().join(".gitignore"), &content).unwrap();
        let mut rules = Ignore::new(Case::Sensitive, Limits::default());
        rules.add(Source::Directory(b""), &content).unwrap();
        let paths: Vec<_> = (0..1000)
            .map(|n| {
                match n % 4 {
                    0 => format!("generated-{}.tmp", n % patterns),
                    1 => format!("module-{}/x/file.cache", n % patterns),
                    2 => format!("src/file-{n}.rs"),
                    _ => "keep.tmp".to_owned(),
                }
                .into_bytes()
            })
            .collect();
        let input = paths
            .iter()
            .flat_map(|p| p.iter().copied().chain([0]))
            .collect();
        Self {
            root,
            rules,
            paths,
            input,
        }
    }
    fn batch(&self, git: bool) -> Vec<u8> {
        let mut command = if git {
            let mut command = Command::new("git");
            command.args([
                "-c",
                "core.excludesFile=",
                "-c",
                "core.ignoreCase=false",
                "check-ignore",
                "--no-index",
                "-z",
                "--stdin",
            ]);
            command
        } else {
            let executable = std::env::current_exe().unwrap();
            let release = executable.parent().unwrap().parent().unwrap();
            let mut command = Command::new(
                release
                    .join("examples")
                    .join(format!("ignore_batch{}", std::env::consts::EXE_SUFFIX)),
            );
            command.arg(".gitignore");
            command
        };
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .current_dir(self.root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.path().join("absent"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Write concurrently: batches and outputs may exceed either platform's pipe capacity.
        let mut stdin = child.stdin.take().unwrap();
        let output = std::thread::scope(|scope| {
            scope.spawn(move || stdin.write_all(&self.input).unwrap());
            child.wait_with_output().unwrap()
        });
        assert!(output.status.success(), "{:?}", output);
        output.stdout
    }
    fn library(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for path in &self.paths {
            if self
                .rules
                .check(path, false)
                .unwrap()
                .is_some_and(|m| m.ignored)
            {
                output.extend_from_slice(path);
                output.push(0);
            }
        }
        output
    }
}
fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("ignore");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(1000));
    for format in ["sha1", "sha256"] {
        for size in [100, 1000] {
            let fixture = Fixture::new(size, format);
            let expected = fixture.batch(true);
            assert_eq!(fixture.library(), expected);
            assert_eq!(fixture.batch(false), expected);
            let label = format!("{format}/{}-rules", size * 2 + 1);
            group.bench_with_input(BenchmarkId::new("library", &label), &fixture, |b, f| {
                b.iter(|| std::hint::black_box(f.library()))
            });
            group.bench_with_input(BenchmarkId::new("git-process", &label), &fixture, |b, f| {
                b.iter(|| std::hint::black_box(f.batch(true)))
            });
            group.bench_with_input(
                BenchmarkId::new("girt-process", &label),
                &fixture,
                |b, f| b.iter(|| std::hint::black_box(f.batch(false))),
            );
        }
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
