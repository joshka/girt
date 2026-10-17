# Push Delta Evidence

## Result

A push of an eight-byte edit to a deterministic 512 KiB blob on the large fixture produces a
598-byte pack, compared with main's 525,690 bytes and Git's 310 bytes. The pack-size budget of at
most twice Git is met (1.93×). After the review correction preserves compressed packed-whole
fallbacks, local edit latency is 0.22 s versus Git's 0.10 s, above the 2× target; HTTP edit latency
is 0.19 s versus 0.18 s. Native new-branch median is 0.18 s versus main's 0.19 s. Earlier repeats
ranged from no new-branch regression to a 15% increase, so the latest repeat alone does not
establish a stable regression budget. Native peak RSS remains below 128 MiB. These results establish
this workload's pack-size behavior, not parity with Git's heuristics, a latency improvement or an
end-to-end jj result.

## Specification and Implementation

The [issue specification](https://github.com/joshka/girt/issues/25#issuecomment-6124134524) precedes
implementation. Acceptance commit `c805d4b2` fails both small-edit cases on main `608f4dd0`: 524,694
bytes for SHA-1 and 524,749 bytes for SHA-256. Depth-zero and unadvertised-base cases pass. The
[design round](https://github.com/joshka/girt/issues/25#issuecomment-6124203752) rejects
internal-only search and stored-delta-only reuse because neither covers a new loose edit. The
[measured extension](https://github.com/joshka/girt/issues/25#issuecomment-6124500162) adds tree
bases and early termination after a strong match.

Graph marks provide candidate blob/tree IDs and their advertised root proof. Candidate payloads are
read through normal pinned-store lookup with identity checking. Only bases actually emitted add root
dependencies to the existing live advertisement check. Default compression searches earlier output
entries and verified receiver candidates; explicit ordinary compression remains available. REF_DELTA
works with or without an OFS advertisement. External bases have output depth zero; internal/output
chains obey the read bound and enabled compression depth bound. Packs offering receiver candidates
cap depth at one, so internal bases resolve as ordinary entries before external lookup.
`PushOptions::with_compression` exposes ordinary encoding through the configured workflow as well as
the existing low-level policy. Receiver import reconstructs and repairs thin packs, checks
connectivity, then installs before conditional ref publication. Source CRC, typed edges, source
identity reads, budgets, cancellation, native knowledge retry, no-clobber installation and CAS
remain.

Candidate values count against `max_input_bytes` and the per-object read/write bounds. Inadequate
candidate budget falls back to whole objects. Internal and external searches share each target's
candidate/work allowance. Metadata follows the bounded graph marks and is loaded by object kind;
search retains its existing fresh window and one external payload. This is not a process heap cap.

## Large Fixture Measurements

macOS 26.6.2 (25G83), Apple M2 Max, Rust 1.99.0, Git 2.56.0, release build with debug information.
The source fixture is `/tmp/opus-med-23/source.git`, the previously retained bare jj clone with
131,152 objects, 325 refs and a roughly 200 MiB pack. Each source and receiver starts with a fresh
`cp -cR` of that fixture. Original push objects are overlaid from the retained perf-audit fixture;
the large edit adds an independently Git-written blob and two commits without modifying jj. All data
stays in disposable copies. Both Git and girt use `GIT_CONFIG_GLOBAL=/dev/null`.

Cells are median wall time (three-run full range), then maximum peak RSS from `/usr/bin/time -l`.
Runs rotate main, changed and Git. Warm filesystem cache is intentional; the first-launch range
includes macOS executable startup overhead. Every timing process holds `/tmp/girt-timing.lock`. Git
HTTP uses the checked-in loopback `http_server.py` and `git http-backend`; the owned server is
terminated and waited on in `finally`. Every destination passes Git connectivity checking and edit
runs compare exact blob bytes. This does not claim cold-storage behavior.

Measurements below are from the review follow-up `ceafb46c`.

| Workload           | Main `608f4dd0`              | Changed                      | Git                          |
| ------------------ | ---------------------------- | ---------------------------- | ---------------------------- |
| local new branch   | 0.19 s (0.17–0.50), 90.8 MiB | 0.18 s (0.18–0.56), 91.9 MiB | 0.07 s (0.07–0.09), 14.4 MiB |
| local fast-forward | 0.19 s (0.18–0.20), 90.9 MiB | 0.17 s (0.17–0.18), 91.0 MiB | 0.07 s (0.07–0.08), 14.4 MiB |
| local 512 KiB edit | 0.19 s (0.18–0.19), 91.0 MiB | 0.22 s (0.22–0.23), 90.9 MiB | 0.10 s (0.09–0.10), 15.9 MiB |
| http new branch    | 0.14 s (0.13–0.15), 26.1 MiB | 0.15 s (0.14–0.15), 26.5 MiB | 0.15 s (0.14–0.15), 10.7 MiB |
| http fast-forward  | 0.14 s (0.14–0.14), 26.1 MiB | 0.15 s (0.14–0.15), 26.8 MiB | 0.16 s (0.15–0.16), 11.6 MiB |
| http 512 KiB edit  | 0.16 s (0.16–0.16), 26.9 MiB | 0.19 s (0.18–0.19), 28.1 MiB | 0.18 s (0.17–0.18), 15.8 MiB |

The initial baseline was taken before acceptance implementation. An intermediate blob-only version
sent 1,568 bytes and regressed median latency by trying further candidates. The earlier tree-search
repeat precedes the per-target counter and one-level thin-output corrections. Final samples include
substantial system/runtime variation; no outliers are discarded. Raw per-run files are local
measurement outputs and are not checked into the repository.

## Reproduction

The [driver](push-deltas/driver.rs) and [measurement script](push-deltas/measure.py) retain the
measurement procedure. The script targets this macOS fixture environment and writes raw logs and
JSON to `/tmp/girt-25-evidence`, outside the checkout. It creates the deterministic edit fixture,
runs all three implementations over fresh APFS copies, verifies receiver connectivity and exact edit
bytes, and records preparation phases and Git pack bytes. It requires these retained fixtures:

- `/tmp/opus-med-23/source.git`: the bare large fixture described above.
- `/tmp/perf-audit/pushsrc.git`: the original push overlay containing commits
  `987c084b8f3ff28e8246d8b38c55c323c85611d5` and `812a3cdbfb334543bbd5858335e21fc5c941287b`.

Create a temporary Cargo binary project at `/tmp/girt-25-driver`, copy `driver.rs` to its
`src/main.rs`, and depend on `girt` with `features = ["http"]` and a path to the checkout being
measured. Set `[profile.release] debug = 1`. Build against main `608f4dd0` first and save the binary
as `/tmp/girt-25-evidence/main-driver`; then rebuild against this change, leaving the changed binary
at `/tmp/girt-25-driver/target/release/perfdrv`. Build before acquiring the timing lock. The
script's HTTP server path assumes `/Users/joshka/local/girt/work/25-push-deltas`; adjust it for
another checkout. Use a new empty output directory for each complete repeat.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
unset RUSTUP_TOOLCHAIN
export GIT_CONFIG_GLOBAL=/dev/null
mkdir -p /tmp/girt-25-evidence
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
release_lock() {
    python3 -c 'from pathlib import Path; Path("/tmp/girt-timing.lock").unlink(missing_ok=True)'
}
trap release_lock EXIT
python3 docs/evidence/push-deltas/measure.py
```

Run `cargo bench --bench push -- push-blob-edit` under the same lock to reproduce the smaller
Criterion comparison. The retained fixture paths are prerequisites; the script does not download or
modify an upstream repository.

## Profile and Criterion

Main preparation on the large edit spent 8.66 ms opening the store, 6.34 ms selecting and 8.39 ms
writing, before receiver work. A 200-preparation repeat sampled at 1 ms shows 172 deflate-medium and
58 longest-match self samples, versus 36 SHA-1, eight write and six read samples. Loader samples are
excluded. These are profiler self-sample counts; instrumented wall time is not a benchmark sample.
Final phase samples report main selection/writing 7.97/8.16 ms and changed selection/writing
2.11/35.67 ms. Smaller transfer size adds search and receiver thin-pack repair work, so pack bytes
alone do not establish a latency improvement.

`benches/push.rs` adds a reproducible Criterion group over a 512 KiB original deterministic blob,
one eight-byte edit and an explicitly advertised blob root. Setup is outside the measured operation.
Under the same shared lock, 30 samples, one-second warmup and five-second requested measurement:
ordinary preparation has a Criterion time estimate of 9.764 ms (95% interval 9.729–9.815 ms);
advertised-base preparation is 11.575 ms (11.076–12.267 ms). Criterion reported a sampling warning:
the requested five-second window expanded to 5.275 seconds for 30 linear samples. This is a
benchmark sampling warning, not a compiler/check warning. The large-fixture phase also enumerates
boundary metadata; this small group isolates base search rather than claiming to represent every
preparation cost.

## Validation and Limits

The acceptance suite covers both hashes, strict Git thin-pack repair, exact content/ref publication,
REF framing with and without OFS advertisement, packed-source edits, empty receiver internal bases,
zero/one output depth, ordinary policy, input/work fallback and corrupt candidate refusal with the
receiver unchanged. Configured native, HTTP and SSH pushes reach real Git-compatible receivers; SSH
uses a local client stand-in and starts no sshd. Configured native, HTTP and SSH cases cover both
SHA-1 and SHA-256. A packed-whole fallback test compares complete output bytes against ordinary
compressed reuse in both formats. Existing advertisement and transfer-reuse suites retain shallow
refusal, hidden refs, root-race retry, CRC damage, false index/base identity, connectivity and
no-mutation evidence. The advertisement corpus explicitly selects ordinary compression so
installed-index counts still equal wire-object counts; default delta cases separately check thin
repair, including hidden receiver history. The retry compares incoming IDs against Git's six
selected IDs plus its verified repair base. Windows CI explicitly includes the portable delta suite;
Unix CI runs all features and the SSH stand-in. Platform results remain CI evidence.

The default external search skips payloads under 64 bytes, limits eligible payloads to 1 MiB, tries
nearest-size candidates, and stops after a completed delta below 1% of target size. Larger objects,
unmatched candidates and exhausted search use whole-object output. Packed whole entries keep their
CRC-checked stored compressed stream when search produces no delta, and objects under 64 bytes skip
reconstruction for search. Corrupt offered candidates deliberately refuse preparation even when a
different candidate might work; this strictness is documented in `PushLimits::compression`. Git can
find better deltas; pathname-oriented matching and broader size/work heuristics remain possible
follow-ups. Native import repairs thin packs by appending ordinary bases, potentially duplicating an
already stored base. Full `just check` exposed a receiver resolution ambiguity when a physically
present, unreferenced object was both a pending internal delta target and a candidate for
store-assisted external lookup. The existing native root-change retry failed with duplicate identity
before any mutation. The
[recorded correction](https://github.com/joshka/girt/issues/25#issuecomment-6124896054) caps
candidate packs to one delta level, eliminating that shape by construction. Both-format
interrupted/rejected receive cases and the existing retry assert successful exact publication.
Broader multi-level thin import resolution remains a follow-up. Shallow push negotiation and
concurrent server GC protection remain outside this increment.

The final local gate is `just check` under the shared timing lock, with warnings rejected. Its
result and exact published revision are recorded in the PR; this report's measurements describe the
review follow-up algorithm, including preserved compressed fallback entries.
