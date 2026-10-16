"""Exercise the publishing gate without contacting GitHub or waiting."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("wait-for-release-ci.sh").resolve()


class ReleaseCiGate(unittest.TestCase):
    def run_gate(self, runs, api_exit=0, pages=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "response.json").write_text(json.dumps(pages if pages is not None else [{"workflow_runs": runs}]))
            (root / "gh").write_text(
                """#!/bin/sh
printf '%s\n' "$@" > "$ARGS"
cat "$FIXTURE"
exit "$API_EXIT"
"""
            )
            (root / "sleep").write_text("#!/bin/sh\nexit 0\n")
            for name in ("gh", "sleep"):
                (root / name).chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPT)],
                env={
                    **os.environ,
                    "PATH": f"{root}:{os.environ['PATH']}",
                    "FIXTURE": str(root / "response.json"),
                    "API_EXIT": str(api_exit),
                    "ARGS": str(root / "args"),
                    "GITHUB_REPOSITORY": "joshka/girt",
                    "GITHUB_SHA": "expected-sha",
                },
                capture_output=True,
                text=True,
                timeout=30,
            )

            self.assertEqual((root / "args").read_text().splitlines(), [
                "api", "--method", "GET", "--paginate", "--slurp",
                "repos/joshka/girt/actions/workflows/validation.yml/runs",
                "-f", "head_sha=expected-sha", "-f", "event=push",
                "-f", "branch=main", "-f", "per_page=100",
            ])
            return result

    def successful_run(self, **overrides):
        return {
            "id": 1,
            "head_sha": "expected-sha",
            "event": "push",
            "head_branch": "main",
            "path": ".github/workflows/validation.yml",
            "head_repository": {"full_name": "joshka/girt"},
            "status": "completed",
            "conclusion": "success",
            "html_url": "https://example.test/run/1",
            **overrides,
        }

    def test_exact_full_push_success(self):
        self.assertEqual(self.run_gate([self.successful_run()]).returncode, 0)

    def test_success_on_later_page(self):
        result = self.run_gate([], pages=[
            {"workflow_runs": [self.successful_run(head_sha="another-sha")]},
            {"workflow_runs": [self.successful_run()]},
        ])
        self.assertEqual(result.returncode, 0)

    def test_unsuccessful_conclusions_block(self):
        for conclusion in ("failure", "cancelled", "skipped", "timed_out", None):
            with self.subTest(conclusion=conclusion):
                result = self.run_gate([self.successful_run(conclusion=conclusion)])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("did not succeed", result.stdout)

    def test_other_identity_cannot_authorize(self):
        for override in (
            {"head_sha": "another-sha"},
            {"event": "workflow_dispatch"},
            {"event": "pull_request"},
            {"head_branch": "topic"},
            {"path": ".github/workflows/other.yml"},
            {"head_repository": {"full_name": "other/girt"}},
            {"status": "in_progress"},
        ):
            with self.subTest(override=override):
                result = self.run_gate([self.successful_run(**override)])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Timed out", result.stdout)

    def test_missing_ci_times_out(self):
        self.assertNotEqual(self.run_gate([]).returncode, 0)

    def test_api_error_blocks_even_with_success_body(self):
        self.assertNotEqual(self.run_gate([self.successful_run()], api_exit=1).returncode, 0)

    def test_newer_failed_run_overrides_old_success(self):
        result = self.run_gate([
            self.successful_run(),
            self.successful_run(id=2, conclusion="failure"),
        ])
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
