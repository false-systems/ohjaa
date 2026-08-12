"""Tests for ohjaa.

The script has no .py extension (it is a CLI), so it is loaded by path.
Everything here is stdlib and offline: no repo, no toimija, no network.
"""

import importlib.util
import json
import sys
import tempfile
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent.parent / "ohjaa"


def load():
    spec = importlib.util.spec_from_loader("ohjaa", SourceFileLoader("ohjaa", str(SCRIPT)))
    module = importlib.util.module_from_spec(spec)
    sys.modules["ohjaa"] = module  # dataclasses resolves annotations via sys.modules
    spec.loader.exec_module(module)
    return module


ohjaa = load()


class ScopesIntersect(unittest.TestCase):
    def test_identical_paths_intersect(self):
        self.assertTrue(ohjaa.scopes_intersect(["src/gates.rs"], ["src/gates.rs"]))

    def test_disjoint_paths_do_not(self):
        self.assertFalse(ohjaa.scopes_intersect(["src/a.rs"], ["src/b.rs"]))

    def test_a_directory_contains_its_files(self):
        self.assertTrue(ohjaa.scopes_intersect(["docs/"], ["docs/proposals/0003.md"]))

    def test_undeclared_scope_intersects_everything(self):
        # An empty scope is undeclared, not empty. Over-report rather than miss.
        self.assertTrue(ohjaa.scopes_intersect([], ["src/a.rs"]))
        self.assertTrue(ohjaa.scopes_intersect(["src/a.rs"], []))

    def test_cross_repo_absolute_scope_reaches_in(self):
        # A sibling in another repo can declare scope over this one.
        self.assertTrue(
            ohjaa.scopes_intersect(["/Users/x/projects"], ["/Users/x/projects/ahti/PROTOCOL.md"])
        )


class ScanRepo(unittest.TestCase):
    """A failed scan must never look like an empty one."""

    def _run(self, returncode=0, stdout="", stderr=""):
        result = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
        return mock.patch.object(ohjaa.subprocess, "run", return_value=result)

    def test_active_session_is_parsed(self):
        payload = json.dumps({
            "sessions": [{
                "id": "1-2", "agent": ["codex"], "worktree": "/tmp/r", "branch": "main",
                "intent": "work", "scope": ["src/"], "liveness": "active",
                "last_seen_at": 10,
            }],
            "warnings": ["overlap"],
            "coverage": {"status": "available"},
        })
        with self._run(stdout=payload):
            scan = ohjaa.scan_repo(Path("/tmp/r"))
        self.assertEqual(scan.coverage, "available")
        self.assertEqual(scan.warnings, ["overlap"])
        self.assertEqual(len(scan.sessions), 1)
        session = scan.sessions[0]
        self.assertEqual((session.agent, session.branch, session.liveness),
                         ("codex", "main", "active"))

    def test_nonzero_exit_is_failed_coverage_not_empty(self):
        with self._run(returncode=128, stderr="not a Git repository"):
            scan = ohjaa.scan_repo(Path("/tmp/r"))
        self.assertEqual(scan.coverage, "failed")
        self.assertEqual(scan.sessions, [])
        self.assertIn("not a Git repository", scan.error)

    def test_unparseable_output_is_failed_coverage(self):
        with self._run(stdout="{not json"):
            scan = ohjaa.scan_repo(Path("/tmp/r"))
        self.assertEqual(scan.coverage, "failed")
        self.assertIsNotNone(scan.error)

    def test_missing_binary_does_not_raise(self):
        with mock.patch.object(ohjaa.subprocess, "run", side_effect=FileNotFoundError("toimija")):
            scan = ohjaa.scan_repo(Path("/tmp/r"))
        self.assertEqual(scan.coverage, "failed")

    def test_toimija_reported_failed_coverage_is_preserved(self):
        payload = json.dumps({"sessions": [], "coverage": {"status": "failed"}})
        with self._run(stdout=payload):
            scan = ohjaa.scan_repo(Path("/tmp/r"))
        self.assertEqual(scan.coverage, "failed")


class Roots(unittest.TestCase):
    def test_only_toimija_prepared_dirs_are_found(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "prepared" / ".toimija").mkdir(parents=True)
            (root / "plain").mkdir()
            found = ohjaa.toimija_repos([root])
        self.assertEqual([p.name for p in found], ["prepared"])

    def test_a_root_may_itself_be_a_repo(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".toimija").mkdir()
            found = ohjaa.toimija_repos([root])
        self.assertEqual(found, [root])

    def test_repeated_roots_are_not_double_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".toimija").mkdir()
            found = ohjaa.toimija_repos([root, root])
        self.assertEqual(len(found), 1)

    def test_explicit_roots_win_over_config(self):
        self.assertEqual(ohjaa.load_roots(["/tmp/x"]), [Path("/tmp/x")])


class Formatting(unittest.TestCase):
    def test_ago_never_claims_freshness_it_lacks(self):
        self.assertEqual(ohjaa.ago(-1), "never")
        self.assertEqual(ohjaa.ago(5), "5s ago")
        self.assertEqual(ohjaa.ago(600), "10m ago")
        self.assertEqual(ohjaa.ago(7200), "2h ago")


class Policy(unittest.TestCase):
    def test_choose_seat_is_unimplemented_on_purpose(self):
        # Guards the human's decision: nobody should quietly fill this in.
        request = ohjaa.Request("claude", "why", [], Path("/tmp/r"))
        with self.assertRaises(NotImplementedError):
            ohjaa.choose_seat(request, [])


if __name__ == "__main__":
    unittest.main()
