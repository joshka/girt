#!/usr/bin/env python3
"""Measure girt's quality scorecard; see docs/quality.md for the metrics and targets.

Usage: scripts/quality-scorecard.py [--coverage] [--mutants] [--full] [--output PATH]

The default run measures static, Clippy and Rustdoc metrics in a few minutes. `--coverage` and
`--mutants` add the slow measurements; their last results are cached under target/quality/ and
reused by later default runs, marked with the date they were measured.
"""

import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / "target" / "quality"
LOCK = Path("/tmp/girt-timing.lock")

# Capabilities measured for correctness and robustness. Each lists the source and integration test
# paths that own it (prefixes match directories and file stems) and the properties that apply:
#   format:    Git byte format girt writes; needs a round-trip or Git-bytes test.
#   parser:    decodes repository or network input; needs malformed-input tests and no panics.
#   bounded:   reads untrusted or unbounded input; needs limit tests.
#   cancel:    long-running operation; needs cancellation tests.
#   mutating:  publishes or deletes stored data; needs crash-ordering or fault tests.
CAPABILITIES = [
    ("blobs and object IDs", ["src/object.rs"], ["blobs", "object_ids", "sha256"], "format parser"),
    ("trees", ["src/tree.rs"], ["trees"], "format parser"),
    ("commits", ["src/commit"], ["commits"], "format parser"),
    ("annotated tags", ["src/tag.rs", "src/peel.rs"], ["tags"], "format parser"),
    ("loose objects", ["src/loose.rs"], ["decoding"], "format parser bounded mutating"),
    (
        "pack reading",
        ["src/objects", "src/pack/mod.rs", "src/pack/reader.rs", "src/pack/index.rs", "src/pack/delta.rs",
         "src/pack/file.rs", "src/pack/tests.rs"],
        ["packs", "alternates", "pack_index_integrity"],
        "parser bounded",
    ),
    (
        "pack writing",
        ["src/pack/write.rs", "src/pack/compression.rs", "src/pack/reuse.rs"],
        ["packs", "push_deltas", "transfer_reuse"],
        "format bounded cancel mutating",
    ),
    ("pack index rebuild", ["src/pack/rebuild.rs"], ["pack_index_rebuild"], "format bounded cancel mutating"),
    (
        "files references",
        ["src/refs/mod.rs", "src/refs/name.rs", "src/refs/packed.rs", "src/refs/store.rs",
         "src/refs/transaction", "src/refs/transaction_tests.rs",
         "src/refs/transaction_stored_tests.rs", "src/refs/transaction_wait_tests.rs", "src/refs/enumerate.rs", "src/refs/lock_wait.rs", "src/refs/leaf_symlink",
         "src/refs/delete_tests.rs", "src/refs/observation_tests.rs"],
        ["references", "references_portable"],
        "format parser bounded mutating",
    ),
    # Reflog writes are published by reference transactions, which own their fault tests.
    ("reflogs", ["src/refs/reflog.rs", "src/repository/reflog_policy.rs", "src/refs/transaction",
                 "src/refs/transaction_tests.rs", "src/refs/transaction_stored_tests.rs"], ["imported_reflogs", "reflog_policy"],
     "format parser bounded mutating"),
    ("reftable", ["src/refs/reftable", "src/refs/imported"], ["reftable"], "format parser bounded mutating"),
    ("configuration", ["src/config"], ["config_continuation", "config_edit", "config_resolution"],
     "format parser bounded mutating"),
    (
        "repository discovery and init",
        ["src/repository.rs", "src/repository/discover.rs", "src/repository/init.rs", "src/repository/shallow.rs",
         "src/repository/command_layout"],
        ["repositories", "discovery", "repository_location", "repository_metadata", "layout_shallow"],
        "format parser bounded mutating",
    ),
    ("worktrees", ["src/repository/worktrees.rs", "src/repository/worktree_admin.rs",
                   "src/repository/worktree_create.rs", "src/repository/worktree_repair.rs"], ["worktree_admin", "worktree_create", "worktree_policy",
                                                "worktree_repair"], "parser mutating"),
    ("colocation", ["src/repository/colocation.rs", "src/repository/operation.rs"], ["colocation"], "mutating"),
    ("commit history", ["src/history.rs", "src/edges.rs"], ["history", "walk_integrity"], "bounded cancel"),
    ("pkt-line protocol", ["src/packet.rs", "src/transfer.rs"], ["fetch", "push_advertisement"],
     "parser bounded cancel"),
    ("fetch", ["src/fetch"], ["fetch", "fetch_workflow", "local_native"], "bounded cancel mutating"),
    ("push", ["src/push"], ["push", "push_advertisement", "push_deltas"], "bounded cancel mutating"),
    ("smart HTTP", ["src/transport/http", "src/transport/control.rs", "src/transport/mod.rs"],
     ["http", "http_portable"], "parser bounded cancel"),
    ("SSH", ["src/transport/ssh", "src/transport/process"], ["ssh"], "parser bounded cancel"),
    ("remotes and credentials", ["src/remote"], ["remotes", "remote_url", "credential_store"], "parser mutating"),
    ("clone", ["src/clone"], ["clone"], "bounded cancel mutating"),
    ("tree comparison", ["src/tree_compare"], ["tree_compare"], "bounded cancel"),
    ("rewrite detection", ["src/rewrites"], ["rewrites"], "bounded cancel"),
    ("content diff", ["src/content_diff"], ["content_diff"], "bounded cancel"),
    ("index", ["src/index"], ["index", "refresh"], "format parser bounded cancel mutating"),
    ("status", ["src/status"], ["status", "status_portable"], "bounded cancel"),
    ("checkout", ["src/checkout", "src/file_policy.rs"], ["checkout", "checkout_portable"], "bounded cancel mutating"),
    ("ignore rules", ["src/ignore"], ["ignore"], "parser"),
    ("retention and repack", ["src/retention"], ["retention", "retention_integrity", "retention_noop",
                                                 "retention_repack_contracts", "retention_survivors",
                                                 "fetch_retention"], "bounded cancel mutating"),
]

# Core modules sampled for mutation testing: object formats whose bugs corrupt or misread data.
MUTANT_FILES = ["src/object.rs", "src/tree.rs", "src/commit.rs", "src/commit/payload.rs", "src/tag.rs", "src/packet.rs"]
# A fixed quarter of the mutants keeps the run near 20 minutes; rotate the shard to widen coverage.
MUTANT_SHARD = "0/4"
MUTANT_TESTS = ["--lib", "--test", "blobs", "--test", "trees", "--test", "commits", "--test", "tags",
                "--test", "object_ids", "--test", "sha256"]

ORACLE = re.compile(r'Command::new\("git"\)|\w+_git::|mod \w+_git\b|support/\w+_git\.rs"|\bgit::git\(')
SHA256 = re.compile(r"Sha256|sha256")
# Test-name evidence. A pattern matches whole underscore-separated components of a test name or
# its named `rstest` case (`round_trip` matches `index_round_trips`, `fault` does not match
# `default`), so each pattern is a component prefix.
ROUND_TRIP = ["round_trip", "roundtrip", "bytes", "byte_for_byte", "matches_git", "identical", "canonical",
              "encod", "serializ", "git_bytes"]
MALFORMED = ["reject", "malformed", "truncat", "invalid", "corrupt", "damaged", "refuse", "overflow",
             "unterminated", "missing", "duplicate", "unsupported", "bad", "garbage", "empty", "nul"]
LIMIT = ["limit", "budget", "bound", "exceed", "too_large", "too_long", "too_deep", "too_many", "maximum"]
CANCEL = ["cancel", "interrupt", "deadline", "timeout", "abort"]
# Crash ordering means an injected failure or interruption around publication, or recovery from
# one; rejecting bad input before publishing is malformed-input evidence, not crash evidence.
CRASH = ["crash", "interrupt", "torn", "fsync", "sync_order", "kill", "fault", "failpoint", "injected",
         "leftover", "recover", "rollback", "partial_write", "power_loss", "dropping", "failed", "sync",
         "publication_failure", "log_failure", "cleans_temporary"]

# Tests whose names do not match the vocabulary above but provide the evidence, checked by review.
EVIDENCE = {
    ("annotated tags", "round_trip"): ["agrees_with_git_in_both_directions"],
}

PANIC_LINTS = ["unwrap_used", "expect_used", "panic", "unreachable", "todo", "unimplemented",
               "indexing_slicing"]
# Git's own terms such as `git_dir` are fine; these name libgit2, gix or CLI concepts instead.
FOREIGN_NAMES = re.compile(r"^get_|revparse|rev_parse|porcelain|_opts$|^oid$|_oid$|^odb|_odb$|"
                           r"^repo_|libgit2|gix")


def log(message):
    print(f"[scorecard] {message}", file=sys.stderr, flush=True)


def run(command, *, ok=(0,), **kwargs):
    """Run a command, failing the scorecard unless it exits with one of the `ok` codes, so a broken
    tool never reads as a clean measurement."""
    log(" ".join(command))
    try:
        result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, **kwargs)
    except FileNotFoundError as error:
        raise SystemExit(f"[scorecard] cannot run {command[0]}: {error}") from error
    if result.returncode not in ok:
        raise SystemExit(f"[scorecard] {' '.join(command)} exited {result.returncode}\n"
                         f"{result.stderr[-4000:]}{result.stdout[-2000:]}")
    return result


@contextmanager
def timing_lock():
    """Hold the machine-wide timing lock only while a heavy command runs."""
    while subprocess.run(["shlock", "-f", str(LOCK), "-p", str(os.getpid())]).returncode != 0:
        time.sleep(15)
    try:
        yield
    finally:
        LOCK.unlink(missing_ok=True)


def owned(path, prefixes):
    """Whether `path` is one of `prefixes`, a file below one, or the `.rs` root of a module directory."""
    return any(path in (p, p + ".rs") or path.startswith(p + "/") for p in prefixes)


def source_files():
    return sorted(str(p.relative_to(ROOT)) for p in (ROOT / "src").rglob("*.rs"))


def test_names(text):
    """Test scenarios in a Rust source file: one `name` per `#[test]` function, and one `name case`
    per named case of an `#[rstest]` function (`name` alone when it has no named cases).

    Attributes may span lines, so each one is consumed by bracket depth rather than by line."""
    names = []
    for start in re.finditer(r"#\[(test|rstest)\]", text):
        position, cases = start.end(), []
        while True:
            match = re.compile(r"\s*#\[").match(text, position)
            if not match:
                break
            depth, index = 1, match.end()
            while depth and index < len(text):
                depth += {"[": 1, "]": -1}.get(text[index], 0)
                index += 1
            case = re.match(r"case::(\w+)", text[match.end():index])
            if case:
                cases.append(case.group(1))
            position = index
        function = re.compile(r"\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)").match(text, position)
        if function:
            names += [f"{function.group(1)} {case}" for case in cases] or [function.group(1)]
    return names


def matches(name, patterns):
    """Whether any pattern starts at a component boundary of a test name."""
    return any(re.search(rf"(?:^|[_ ]){re.escape(p)}", name) for p in patterns)


def evidence(capability, kind, names, patterns):
    aliases = EVIDENCE.get((capability, kind), [])
    return sum(matches(n, patterns) or n.split(" ")[0] in aliases for n in names)


def read(path):
    return (ROOT / path).read_text(errors="replace")


def capability_tests():
    sources = source_files()
    rows = []
    for name, src_prefixes, tests, flags in CAPABILITIES:
        src = [s for s in sources if owned(s, src_prefixes)]
        test_files = [f"tests/{t}.rs" for t in tests if (ROOT / f"tests/{t}.rs").exists()]
        texts = {p: read(p) for p in src + test_files}
        names = [n for p in texts for n in test_names(texts[p])]
        rows.append({
            "name": name,
            "flags": set(flags.split()),
            "src": src,
            "oracle": any(ORACLE.search(texts[p]) for p in test_files),
            "sha256": any(SHA256.search(texts[p]) for p in test_files) or
                      any(SHA256.search(n) for n in names),
            "round_trip": evidence(name, "round_trip", names, ROUND_TRIP),
            "malformed": evidence(name, "malformed", names, MALFORMED),
            "limit": evidence(name, "limit", names, LIMIT),
            "cancel": evidence(name, "cancel", names, CANCEL),
            "crash": evidence(name, "crash", names, CRASH),
            "tests": len(names),
        })
    return rows


def clippy_counts(lints):
    """Count Clippy warnings for library code only (no test modules) by lint and file."""
    args = ["cargo", "clippy", "--lib", "--all-features", "--message-format=json", "--quiet", "--",
            "-A", "clippy::all"] + [arg for lint in lints for arg in ("-W", lint)]
    with timing_lock():
        result = run(args)
    counts = {}
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        diag = message.get("message") if message.get("reason") == "compiler-message" else None
        if not diag or not (diag.get("code") or {}).get("code", "").startswith("clippy::"):
            continue
        spans = [s for s in diag["spans"] if s.get("is_primary")]
        file = spans[0]["file_name"] if spans else "?"
        counts.setdefault(diag["code"]["code"].replace("clippy::", "", 1), []).append(file)
    return counts


def rustdoc_json():
    with timing_lock():
        run(["cargo", "+nightly", "rustdoc", "--lib", "--all-features", "--", "-Z", "unstable-options",
             "--output-format", "json"])
    return json.loads((ROOT / "target/doc/girt.json").read_text())


def public_api(doc):
    """Publicly reachable items: items with public paths, their fields and variants, and inherent
    methods. Trait impl members are excluded because their documentation belongs to the trait."""
    index = doc["index"]
    root = {k for k, v in doc["paths"].items() if v["crate_id"] == 0 and k in index}
    items = {k: index[k] for k in root}
    types = {}
    for key in root:
        item = index[key]
        kind = next(iter(item["inner"]))
        if kind in ("struct", "enum", "union"):
            types[key] = item
            inner = item["inner"][kind]
            members = inner.get("variants", [])
            fields = inner.get("kind", {})
            if isinstance(fields, dict):
                plain = fields.get("plain", {})
                members += plain.get("fields", []) if isinstance(plain, dict) else []
            for member in members:
                child = index.get(str(member))
                if child and child.get("visibility") in ("public", "default"):
                    items[str(member)] = child
        if kind in ("struct", "enum", "union", "trait"):
            for impl_id in item["inner"][kind].get("impls", []) + item["inner"][kind].get("items", []):
                impl = index.get(str(impl_id))
                if not impl:
                    continue
                if "impl" in impl["inner"] and impl["inner"]["impl"]["trait"] is None:
                    for method in impl["inner"]["impl"]["items"]:
                        child = index.get(str(method))
                        if child and child["visibility"] == "public":
                            items[str(method)] = child
                elif kind == "trait" and "function" in impl["inner"]:
                    items[str(impl_id)] = impl
    return items, types


def trait_impls(doc, type_item):
    kind = next(iter(type_item["inner"]))
    names = {}
    for impl_id in type_item["inner"][kind].get("impls", []):
        impl = doc["index"].get(str(impl_id))
        if impl and impl["inner"]["impl"]["trait"]:
            trait = impl["inner"]["impl"]["trait"]["path"].split("::")[-1]
            names[trait] = not impl["inner"]["impl"]["is_negative"]
    return names


def has_example(docs):
    return bool(docs) and "```" in docs


def percent(part, whole):
    return round(100.0 * part / whole, 1) if whole else 100.0


def metric(name, value, target, *, higher=True, unit="", note=None, scored=True, detail=None):
    if target is None:
        status = "ok"
        scored = False
    elif value is None:
        status = "gap"
    elif isinstance(target, bool):
        status = "ok" if value == target else "gap"
    else:
        status = "ok" if (value >= target if higher else value <= target) else "gap"
    entry = {"name": name, "value": value, "target": target, "status": status}
    if unit:
        entry["unit"] = unit
    if note:
        entry["note"] = note
    if detail:
        entry["detail"] = detail
    entry["_scored"] = scored
    entry["_higher"] = higher
    return entry


def attainment(entry):
    value, target = entry["value"], entry["target"]
    if isinstance(target, bool):
        return 1.0 if value == target else 0.0
    if value is None:
        return 0.0
    if entry["_higher"]:
        return min(1.0, value / target) if target else 1.0
    return 1.0 if value <= target else (target + 1) / (value + 1)


def axis(name, target, metrics):
    """Score an axis over all its scored metrics. An unmeasured metric attains 0, so a checkout
    without coverage or mutation results never scores higher than a measured one, and the axis
    is marked incomplete."""
    scored = [m for m in metrics if m["_scored"]]
    measured = [m for m in scored if m["value"] is not None]
    score = round(100 * sum(attainment(m) for m in scored) / len(scored)) if scored else None
    for m in metrics:
        del m["_scored"], m["_higher"]
    return {"axis": name, "score": score, "target": target, "complete": len(measured) == len(scored),
            "measured_metrics": len(measured), "scored_metrics": len(scored), "metrics": metrics}


def coverage_from_cache():
    path = CACHE / "coverage.json"
    if not path.exists():
        return None
    data = json.loads(path.read_text())
    totals = data["data"][0]["totals"]
    return {
        "lines": round(totals["lines"]["percent"], 1),
        "branches": round(totals["branches"]["percent"], 1) if totals.get("branches", {}).get("count") else None,
        "measured": datetime.date.fromtimestamp(path.stat().st_mtime).isoformat(),
    }


def measure_coverage():
    """Measure coverage, replacing the cached result only after the test run succeeds."""
    CACHE.mkdir(parents=True, exist_ok=True)
    fresh = CACHE / "coverage.json.new"
    fresh.unlink(missing_ok=True)
    with timing_lock():
        run(["cargo", "+nightly", "llvm-cov", "--all-features", "--branch", "--json", "--summary-only",
             "--output-path", str(fresh)])
    fresh.replace(CACHE / "coverage.json")


def mutants_from_cache():
    """Summarize the last mutation run, or None when it tested no viable mutant."""
    path = CACHE / "mutants.out" / "outcomes.json"
    if not path.exists():
        return None
    data = json.loads(path.read_text())
    caught, missed, timeout = data.get("caught", 0), data.get("missed", 0), data.get("timeout", 0)
    tested = caught + missed + timeout
    if not tested:
        return None
    missed_list = [o["scenario"]["Mutant"] for o in data.get("outcomes", [])
                   if o.get("summary") == "MissedMutant" and isinstance(o.get("scenario"), dict)]
    return {
        "score": percent(caught + timeout, tested),
        "summary": f"{caught} caught, {timeout} timed out, {missed} missed, {data.get('unviable', 0)} unviable "
                   f"of {data.get('total_mutants', '?')} in shard {MUTANT_SHARD}",
        "missed_examples": [f"{m['file']}:{m['span']['start']['line']} {m.get('name', '')}"[:120]
                            for m in missed_list[:10]],
        "measured": datetime.date.fromtimestamp(path.stat().st_mtime).isoformat(),
    }


def measure_mutants():
    """Run sampled mutation testing, replacing the cached result only after a complete run.

    cargo-mutants exits 2 when mutants were missed and 3 when some timed out; both are results.
    Any other failure, including a failing unmutated baseline (4), is an infrastructure failure."""
    CACHE.mkdir(parents=True, exist_ok=True)
    fresh = CACHE / "new"
    shutil.rmtree(fresh, ignore_errors=True)
    args = ["cargo", "mutants", "--no-shuffle", "--shard", MUTANT_SHARD, "-j", "4", "--timeout-multiplier", "3",
            "--output", str(fresh)]
    for file in MUTANT_FILES:
        args += ["--file", file]
    args += ["--"] + MUTANT_TESTS
    with timing_lock():
        run(args, ok=(0, 2, 3))
    outcomes = json.loads((fresh / "mutants.out" / "outcomes.json").read_text())
    if not outcomes.get("end_time") or not any(o.get("scenario") == "Baseline" and o.get("summary") == "Success"
                                               for o in outcomes.get("outcomes", [])):
        raise SystemExit("[scorecard] cargo mutants did not complete a successful baseline")
    shutil.rmtree(CACHE / "mutants.out", ignore_errors=True)
    (fresh / "mutants.out").replace(CACHE / "mutants.out")
    shutil.rmtree(fresh, ignore_errors=True)


def docs_rs_builds():
    with timing_lock():
        result = run(["cargo", "+nightly", "docs-rs"], ok=range(256),
                     env={**os.environ, "RUSTDOCFLAGS": "-D warnings"})
    return result.returncode == 0


def stale_versions():
    """Places that must name the current release: dependency snippets and the jj status summary."""
    version = re.search(r'^version = "([^"]+)"', read("Cargo.toml"), re.M).group(1)
    major_minor = ".".join(version.split(".")[:2])
    stale = []
    for path in ["README.md", "src/lib.rs"]:
        for found in re.findall(r'girt = "([^"]+)"', read(path)):
            if found != major_minor:
                stale.append(f'{path}: girt = "{found}"')
    for found in re.findall(r"\*\*girt:\*\* (\S+) is published", read("docs/jj-replacement-status.md")):
        if found != version:
            stale.append(f"docs/jj-replacement-status.md: girt {found} is published")
    return stale


def correctness(caps, coverage, mutants):
    formats = [c for c in caps if "format" in c["flags"]]
    no_oracle = [c["name"] for c in caps if not c["oracle"]]
    no_sha256 = [c["name"] for c in caps if not c["sha256"]]
    no_round_trip = [c["name"] for c in formats if not c["round_trip"]]
    metrics = [
        metric("capabilities with Git-oracle integration tests", percent(len(caps) - len(no_oracle), len(caps)),
               100, unit="%", detail=no_oracle),
        metric("capabilities with SHA-256 tests", percent(len(caps) - len(no_sha256), len(caps)), 100, unit="%",
               detail=no_sha256),
        metric("written formats with round-trip or Git-bytes tests",
               percent(len(formats) - len(no_round_trip), len(formats)), 100, unit="%", detail=no_round_trip),
    ]
    note = f"measured {coverage['measured']}" if coverage else "not measured; run with --coverage"
    metrics.append(metric("line coverage", coverage and coverage["lines"], 85, unit="%", note=note))
    metrics.append(metric("branch coverage", coverage and coverage["branches"], 70, unit="%", note=note))
    if mutants:
        metrics.append(metric("mutation score on core object formats", mutants["score"], 80, unit="%",
                              note=f"{mutants['summary']}, measured {mutants['measured']}",
                              detail=mutants["missed_examples"]))
    else:
        metrics.append(metric("mutation score on core object formats", None, 80, unit="%",
                              note="not measured; run with --mutants"))
    return axis("correctness", 90, metrics)


def robustness(caps, panics):
    parsers = [c for c in caps if "parser" in c["flags"]]
    bounded = [c for c in caps if "bounded" in c["flags"]]
    cancel = [c for c in caps if "cancel" in c["flags"]]
    mutating = [c for c in caps if "mutating" in c["flags"]]
    thin = [f"{c['name']} ({c['malformed']})" for c in parsers if c["malformed"] < 5]
    no_limit = [c["name"] for c in bounded if not c["limit"]]
    no_cancel = [c["name"] for c in cancel if not c["cancel"]]
    no_crash = [c["name"] for c in mutating if not c["crash"]]
    parser_files = {f for c in parsers for f in c["src"]}
    explicit = [f for lint in PANIC_LINTS if lint != "indexing_slicing" for f in panics.get(lint, [])]
    in_parsers = [f for f in explicit if f in parser_files]
    indexing = [f for f in panics.get("indexing_slicing", []) if f in parser_files]
    fuzz = len(list((ROOT / "fuzz" / "fuzz_targets").glob("*.rs"))) if (ROOT / "fuzz").exists() else 0
    top = sorted({f: in_parsers.count(f) for f in in_parsers}.items(), key=lambda kv: -kv[1])[:8]
    top_index = sorted({f: indexing.count(f) for f in indexing}.items(), key=lambda kv: -kv[1])[:8]
    metrics = [
        metric("parsers with at least 5 malformed-input tests", percent(len(parsers) - len(thin), len(parsers)),
               100, unit="%", detail=thin),
        metric("fuzz targets", fuzz, len(parsers), scored=False,
               note="deferred until after release (docs/publication-plan.md, roadmap B10)"),
        metric("unwrap/expect/panic/unreachable in parser library code", len(in_parsers), 0, higher=False,
               detail=[f"{f}: {n}" for f, n in top]),
        metric("unchecked indexing or slicing in parser library code", len(indexing), 0, higher=False,
               detail=[f"{f}: {n}" for f, n in top_index]),
        metric("unwrap/expect/panic/unreachable in all library code", len(explicit), None, higher=False),
        metric("bounded capabilities with limit tests", percent(len(bounded) - len(no_limit), len(bounded)), 100,
               unit="%", detail=no_limit),
        metric("long-running capabilities with cancellation tests",
               percent(len(cancel) - len(no_cancel), len(cancel)), 100, unit="%", detail=no_cancel),
        metric("mutating capabilities with crash-ordering or fault tests",
               percent(len(mutating) - len(no_crash), len(mutating)), 100, unit="%", detail=no_crash),
    ]
    return axis("robustness", 90, metrics)


def idiomatic(doc, items, types, style):
    unsafe_sites = [(s, i) for s in source_files() for i, line in enumerate(read(s).splitlines())
                    if re.search(r"\bunsafe\s*(\{|fn|impl)", line)]
    unjustified = [f"{s}:{i + 1}" for s, i in unsafe_sites
                   if "SAFETY:" not in "\n".join(read(s).splitlines()[max(0, i - 12):i])]
    enums = {k: v for k, v in types.items() if "enum" in v["inner"]}
    errors = {k: v for k, v in types.items() if v["name"].endswith("Error")}
    error_enums = [v for v in errors.values() if "enum" in v["inner"]]
    impls = {k: trait_impls(doc, v) for k, v in types.items()}
    not_error = [v["name"] for k, v in errors.items() if not impls[k].get("Error")]
    exhaustive_errors = [v["name"] for v in error_enums if "non_exhaustive" not in json.dumps(v["attrs"])]
    exhaustive_enums = [v["name"] for v in enums.values() if "non_exhaustive" not in json.dumps(v["attrs"])]
    not_send_sync = [v["name"] for k, v in types.items()
                     if not (impls[k].get("Send") and impls[k].get("Sync"))]
    no_debug = [v["name"] for k, v in types.items() if not impls[k].get("Debug")]
    names = {v["name"] for v in items.values() if v.get("name")}
    foreign = sorted(n for n in names if FOREIGN_NAMES.search(n))
    kinds = {}
    for item in items.values():
        kind = next(iter(item["inner"]))
        kinds[kind] = kinds.get(kind, 0) + 1
    metrics = [
        metric("unsafe blocks, functions and impls", len(unsafe_sites), None,
               detail=sorted({s for s, _ in unsafe_sites})),
        metric("unsafe without a SAFETY comment", len(unjustified), 0, higher=False, detail=unjustified),
        metric("public *Error types implementing std::error::Error", percent(len(errors) - len(not_error), len(errors)),
               100, unit="%", detail=not_error),
        metric("public error enums marked #[non_exhaustive]",
               percent(len(error_enums) - len(exhaustive_errors), len(error_enums)), 100, unit="%",
               detail=exhaustive_errors[:20]),
        metric("public enums marked #[non_exhaustive]", percent(len(enums) - len(exhaustive_enums), len(enums)),
               None, unit="%", note="informational; closed sets such as ObjectFormat may stay exhaustive"),
        metric("public types that are Send + Sync", percent(len(types) - len(not_send_sync), len(types)), 100,
               unit="%", detail=not_send_sync[:20]),
        metric("public types implementing Debug", percent(len(types) - len(no_debug), len(types)), 100, unit="%",
               detail=no_debug[:20]),
        metric("CLI-, libgit2- or gix-shaped public names", len(foreign), 0, higher=False, detail=foreign[:20]),
        metric("public API items", len(items), None, detail=[f"{k}: {n}" for k, n in sorted(kinds.items())]),
        metric("clippy::pedantic warnings", sum(len(v) for k, v in style.items() if k in PEDANTIC), None,
               note="informational, not a gate"),
        metric("clippy::nursery warnings", sum(len(v) for k, v in style.items() if k not in PEDANTIC), None,
               note="informational, not a gate"),
    ]
    return axis("idiomatic", 90, metrics)


def documentation(doc, items, docs_rs):
    undocumented = [v["name"] or "?" for v in items.values() if not (v.get("docs") or "").strip()]
    modules = [v for v in items.values() if "module" in v["inner"]]
    root = doc["index"][str(doc["root"])]
    modules_without_docs = [v["name"] for v in modules if not (v.get("docs") or "").strip()]
    modules_without_examples = [v["name"] for v in modules if v is not root and not has_example(v.get("docs"))]
    top_level = [v for v in items.values()
                 if next(iter(v["inner"])) in ("struct", "enum", "trait", "function")
                 and v["span"] and v["visibility"] == "public"]
    with_example = [v for v in top_level if has_example(v.get("docs"))]
    lint = re.search(r"missing_docs\s*=|#!\[(deny|warn|forbid)\([^)]*missing_docs", read("Cargo.toml") + read("src/lib.rs"))
    stale = stale_versions()
    metrics = [
        metric("public items documented", percent(len(items) - len(undocumented), len(items)), 100, unit="%",
               detail=undocumented[:20]),
        metric("missing_docs lint enabled", bool(lint), True),
        metric("crate docs with an example", has_example(root.get("docs")), True),
        metric("public modules documented", percent(len(modules) - len(modules_without_docs), len(modules)), 100,
               unit="%", detail=modules_without_docs),
        metric("public modules with an example",
               percent(len(modules) - 1 - len(modules_without_examples), len(modules) - 1), 100, unit="%",
               detail=modules_without_examples),
        metric("public types, traits and functions with an example", percent(len(with_example), len(top_level)),
               40, unit="%"),
        metric("docs.rs build with warnings denied", docs_rs, True),
        metric("stale release references", len(stale), 0, higher=False, detail=stale),
    ]
    return axis("docs", 90, metrics)


# Membership of clippy::pedantic; anything else in the style run is clippy::nursery.
PEDANTIC = set()


def style_counts():
    global PEDANTIC
    pedantic = clippy_counts(["clippy::pedantic"])
    nursery = clippy_counts(["clippy::nursery"])
    PEDANTIC = set(pedantic)
    return {**pedantic, **{k: v for k, v in nursery.items() if k not in pedantic}}


def table(report):
    lines = ["| Axis | Metric | Value | Target | Status |", "| --- | --- | --- | --- | --- |"]
    for a in report["axes"]:
        for m in a["metrics"]:
            value = "n/a" if m["value"] is None else f"{m['value']}{m.get('unit', '')}"
            target = "—" if m["target"] is None else f"{m['target']}{m.get('unit', '')}"
            label = f"{a['axis']} ({a['score']}{'' if a['complete'] else ', incomplete'})"
            lines.append(f"| {label} | {m['name']} | {value} | {target} | {m['status']} |")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--coverage", action="store_true", help="measure line and branch coverage")
    parser.add_argument("--mutants", action="store_true", help="run sampled mutation testing")
    parser.add_argument("--full", action="store_true", help="both --coverage and --mutants")
    parser.add_argument("--output", default=str(Path.home() / "tmp" / "girt-quality.json"))
    args = parser.parse_args()
    os.chdir(ROOT)
    if args.coverage or args.full:
        measure_coverage()
    if args.mutants or args.full:
        measure_mutants()
    caps = capability_tests()
    panics = clippy_counts([f"clippy::{lint}" for lint in PANIC_LINTS])
    style = style_counts()
    doc = rustdoc_json()
    items, types = public_api(doc)
    report = {
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "revision": run(["git", "rev-parse", "--short", "HEAD"]).stdout.strip(),
        "axes": [
            correctness(caps, coverage_from_cache(), mutants_from_cache()),
            robustness(caps, panics),
            idiomatic(doc, items, types, style),
            documentation(doc, items, docs_rs_builds()),
        ],
    }
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    print(table(report))
    log(f"wrote {output}")


if __name__ == "__main__":
    main()
