"""Focused checks for caller-cost accounting; run with python3 -m unittest discover -s scripts."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("scorecard", Path(__file__).with_name("quality-scorecard.py"))
scorecard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scorecard)


class ErgonomicsTests(unittest.TestCase):
    def test_counts_import_leaves_and_explicit_caller_costs(self):
        source = '''// ignored comment
use girt::{Repository, PackLimits};
use std::sync::atomic::AtomicBool;
let objects = repo.objects(PackLimits::default())?;
let cancel = AtomicBool::new(false);
let name = b"file".to_vec();
let format = girt::ObjectFormat::Sha1;
'''
        self.assertEqual(scorecard.ergonomics_counts(source), {
            "lines": 6, "imported_names": 3, "defaults": 1,
            "never_cancel": 1, "byte_vectors": 1, "object_formats": 1,
            "identity_literals": 0, "commit_fields": 0, "branch_names": 0,
            "boxed_errors": 0, "empty_vectors": 0,
        })

    def test_new_blame_capability_has_measured_code(self):
        tasks = scorecard.ergonomics_tasks()
        blame = next(t for t in tasks if t["task"] == "Blame-style queries")
        self.assertFalse(blame["capability_gap"])
        self.assertEqual(blame["lines"], 6)
        self.assertEqual(len(tasks), 15)

    def test_cost_increase_reduces_attainment(self):
        entry = scorecard.metric("lines", 11, 10, higher=False)
        self.assertEqual(entry["status"], "gap")
        self.assertLess(scorecard.attainment(entry), 1)

    def test_missing_classification_fails_instead_of_improving_score(self):
        from unittest.mock import patch
        with patch.object(scorecard, "read", return_value="\n### Task\n\nUnclassified\n"):
            with self.assertRaisesRegex(ValueError, "classification"):
                scorecard.ergonomics_tasks()

    def test_per_task_regression_cannot_be_offset_by_another_task(self):
        from unittest.mock import patch
        tasks = scorecard.ergonomics_tasks()
        tasks[0]["lines"] += 1
        tasks[1]["lines"] -= 1
        with patch.object(scorecard, "ergonomics_tasks", return_value=tasks):
            report = scorecard.ergonomics()
        metric = next(m for m in report["metrics"] if m["name"] == "per-task caller-cost regressions")
        self.assertEqual(metric["value"], 1)
        self.assertEqual(metric["status"], "gap")

    def test_changed_workload_names_fail_comparison(self):
        from unittest.mock import patch
        tasks = scorecard.ergonomics_tasks()
        tasks[0]["task"] = "Different workload"
        with patch.object(scorecard, "ergonomics_tasks", return_value=tasks):
            with self.assertRaisesRegex(ValueError, "workloads must match"):
                scorecard.ergonomics()

    def test_unmeasured_rust_fence_fails(self):
        from unittest.mock import patch
        text = "\n### Task\n\n<!-- ergonomics-status: supported -->\n```rust\nfn main() {}\n```\n"
        with patch.object(scorecard, "read", return_value=text):
            with self.assertRaisesRegex(ValueError, "unmeasured Rust fence"):
                scorecard.ergonomics_tasks()

    def test_budget_cannot_exceed_dated_baseline(self):
        import json
        from unittest.mock import patch
        original_read = scorecard.read
        budget = json.loads(original_read("docs/ergonomics-budget.json"))
        budget[next(iter(budget))]["lines"] += 1
        def read(path):
            return json.dumps(budget) if path == "docs/ergonomics-budget.json" else original_read(path)
        with patch.object(scorecard, "read", side_effect=read):
            with self.assertRaisesRegex(ValueError, "exceeds dated baseline"):
                scorecard.ergonomics()


if __name__ == "__main__":
    unittest.main()
