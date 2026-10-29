"""Candidate evidence must detect changed or mismatched build outputs."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "release_evidence.py"
SOURCE = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"


class ReleaseEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.binary = self.root / "binary"
        self.binary.write_bytes(b"disposable fixture binary")
        self.lock = self.root / "Cargo.lock"
        self.lock.write_text("fixture lockfile\n")

    def run_tool(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *map(str, args)],
            text=True,
            capture_output=True,
            check=False,
        )

    def record(self, name, slot):
        out = self.root / name
        result = self.run_tool(
            "record", "--binary", self.binary, "--lockfile", self.lock,
            "--out", out, "--commit", SOURCE, "--version", "0.1.1",
            "--target", TARGET, "--slot", slot, "--rustc", "fixture rustc",
            "--cargo", "fixture cargo", "--run-id", "42", "--run-attempt", "1",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return out

    def compare(self, first, second):
        return self.run_tool(
            "compare", "--first", first, "--second", second, "--commit", SOURCE,
            "--version", "0.1.1", "--target", TARGET, "--lockfile", self.lock,
            "--run-id", "42", "--run-attempt", "1",
        )

    def test_matching_independent_builds_are_verified(self):
        first = self.record("first", "first")
        second = self.record("second", "second")
        result = self.compare(first, second)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["result"], "match")

    def test_changed_binary_fails_comparison(self):
        first = self.record("first", "first")
        self.binary.write_bytes(b"different fixture binary")
        second = self.record("second", "second")
        result = self.compare(first, second)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mismatch", result.stderr.lower())

    def test_tampered_archive_is_rejected(self):
        first = self.record("first", "first")
        second = self.record("second", "second")
        with (second / "candidate.tar.gz").open("ab") as archive:
            archive.write(b"tampering")
        result = self.compare(first, second)
        self.assertNotEqual(result.returncode, 0)

    def test_tampered_identity_is_rejected_even_if_both_match(self):
        first = self.record("first", "first")
        second = self.record("second", "second")
        for candidate in (first, second):
            path = candidate / "evidence.json"
            evidence = json.loads(path.read_text())
            evidence["commit"] = "b" * 40
            path.write_text(json.dumps(evidence))
        result = self.compare(first, second)
        self.assertNotEqual(result.returncode, 0)

    def test_duplicate_build_slot_is_rejected(self):
        first = self.record("first", "first")
        second = self.record("second", "first")
        result = self.compare(first, second)
        self.assertNotEqual(result.returncode, 0)

    def test_mismatched_compression_toolchains_are_rejected(self):
        first = self.record("first", "first")
        second = self.record("second", "second")
        path = second / "evidence.json"
        evidence = json.loads(path.read_text())
        evidence["zlib_runtime"] = "different-version"
        path.write_text(json.dumps(evidence))
        result = self.compare(first, second)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("zlib", result.stderr.lower())

    def test_invalid_source_identity_cannot_be_recorded(self):
        result = self.run_tool(
            "record", "--binary", self.binary, "--lockfile", self.lock,
            "--out", self.root / "bad", "--commit", "master", "--version", "0.1.1",
            "--target", TARGET, "--slot", "first", "--rustc", "fixture rustc",
            "--cargo", "fixture cargo", "--run-id", "42", "--run-attempt", "1",
        )
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
