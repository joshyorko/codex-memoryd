"""Publisher guards use only synthetic candidates and temporary files."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


publish = load("release_publish", ROOT / "scripts/release_publish.py")
evidence = load("release_evidence", ROOT / "scripts/release_evidence.py")
COMMIT = "a" * 40
RUN = "42"
ATTEMPT = "1"


class SourceGuardTests(unittest.TestCase):
    def source(self, **changes):
        values = dict(event="workflow_dispatch", ref="refs/tags/v0.1.1", tag="v0.1.1",
                      expected_commit=COMMIT, event_commit=COMMIT, checkout_commit=COMMIT,
                      version="0.1.1")
        values.update(changes)
        return argparse.Namespace(**values)

    def test_accepts_exact_manual_tag_source(self):
        self.assertEqual(publish.validate_source(self.source()), "0.1.1")

    def test_rejects_non_release_source_identity(self):
        bad = [dict(event="push"), dict(ref="refs/heads/master"),
               dict(tag="v0.1.2"), dict(tag="v0.1.1-rc1"),
               dict(expected_commit="a" * 7), dict(event_commit="b" * 40),
               dict(checkout_commit="c" * 40), dict(version="0.1.2")]
        for change in bad:
            with self.subTest(change=change), self.assertRaises(ValueError):
                publish.validate_source(self.source(**change))

    def test_remote_tag_must_still_resolve_to_the_approved_commit(self):
        ref = "refs/tags/v0.1.1"
        publish.validate_remote_tag(f"{COMMIT}\t{ref}\n", "v0.1.1", COMMIT)
        publish.validate_remote_tag(f"{'d' * 40}\t{ref}\n{COMMIT}\t{ref}^{{}}\n", "v0.1.1", COMMIT)
        for output in ("", f"{'b' * 40}\t{ref}\n", f"{COMMIT}\t{ref}\n{COMMIT}\t{ref}\n",
                       f"{COMMIT}\trefs/tags/v0.1.2\n"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                publish.validate_remote_tag(output, "v0.1.1", COMMIT)

    def test_verify_remote_tag_against_local_disposable_git_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            remote = root / "remote.git"
            checkout = root / "checkout"
            subprocess.run(["git", "init", "--bare", str(remote)], check=True, capture_output=True)
            subprocess.run(["git", "init", str(checkout)], check=True, capture_output=True)
            def git(*args):
                return subprocess.run(["git", "-C", str(checkout), *args], check=True,
                                      capture_output=True, text=True).stdout.strip()
            git("config", "user.name", "release test")
            git("config", "user.email", "release-test@example.invalid")
            git("commit", "--allow-empty", "-m", "fixture")
            commit = git("rev-parse", "HEAD")
            git("remote", "add", "origin", str(remote))
            git("tag", "-a", "v0.1.1", "-m", "fixture")
            git("push", "origin", "refs/tags/v0.1.1")
            command = [sys.executable, str(ROOT / "scripts/release_publish.py"),
                       "verify-remote-tag", "--tag", "v0.1.1", "--commit"]
            result = subprocess.run(command + [commit], cwd=checkout, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            result = subprocess.run(command + ["b" * 40], cwd=checkout, capture_output=True)
            self.assertNotEqual(result.returncode, 0)


class WorkflowSafetyTests(unittest.TestCase):
    def events(self, text):
        match = re.search(r"(?m)^on:\n((?:[ \t].*\n|\n)*)", text)
        self.assertIsNotNone(match)
        return re.findall(r"(?m)^  ([a-z_]+):", match.group(1))

    def test_publisher_cannot_be_triggered_by_push_or_pull_request(self):
        text = (ROOT / ".github/workflows/publish-release.yml").read_text()
        self.assertEqual(self.events(text), ["workflow_dispatch"])
        self.assertIn("create-storage-record: false", text)

    def test_pr_preflight_remains_read_only(self):
        text = (ROOT / ".github/workflows/release-preflight.yml").read_text()
        self.assertEqual(set(self.events(text)), {"pull_request", "workflow_dispatch", "workflow_call"})
        for prohibited in ("contents: write", "packages: write", "attestations: write", "id-token: write",
                           "gh release create", "push=true", "push: true", "type=registry"):
            with self.subTest(prohibited=prohibited):
                self.assertNotIn(prohibited, text)


class CandidateGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.lock = self.root / "Cargo.lock"
        self.lock.write_text("locked dependencies\n")
        self.binary = self.root / "binary"
        self.binary.write_bytes(b"only a synthetic binary")
        self.inputs = self.root / "candidates"
        self.inputs.mkdir()
        self.lock_hash = hashlib.sha256(self.lock.read_bytes()).hexdigest()
        for target in evidence.TARGETS:
            for slot in ("first", "second"):
                evidence.record(argparse.Namespace(
                    binary=self.binary, out=self.inputs / f"native-{target}-{slot}",
                    commit=COMMIT, version="0.1.1", target=target, lockfile=self.lock,
                    run_id=RUN, run_attempt=ATTEMPT, slot=slot, rustc="rustc test",
                    cargo="cargo test"))
        for arch in ("amd64", "arm64"):
            for slot in ("first", "second"):
                directory = self.inputs / f"oci-{arch}-{slot}"
                directory.mkdir()
                manifest = "sha256:" + ("1" if arch == "amd64" else "2") * 64
                (directory / "source.json").write_text(json.dumps(dict(
                    commit=COMMIT, version="0.1.1", platform=f"linux/{arch}",
                    slot=slot, run=RUN, attempt=ATTEMPT, lock=self.lock_hash,
                    buildx="buildx test", buildkit="buildkit test",
                    kind="unsigned-nonrelease-candidate")))
                (directory / "build.json").write_text(json.dumps({
                    "containerimage.digest": manifest,
                    "buildx.build.provenance": {"source": "test"}}))
                (directory / "layers.json").write_text(json.dumps({"manifest": manifest}))

    def args(self):
        return argparse.Namespace(candidates=self.inputs, out=self.root / "dist", commit=COMMIT,
                                  version="0.1.1", lockfile=self.lock,
                                  run_id=RUN, run_attempt=ATTEMPT, ref="refs/tags/v0.1.1")

    def test_assembles_only_matching_exact_run_candidates(self):
        publish.verify_candidates(self.args())
        self.assertFalse(self.args().out.exists())
        publish.assemble(self.args())
        dist = self.root / "dist"
        release = json.loads((dist / "provenance.json").read_text())
        self.assertEqual(release["commit"], COMMIT)
        self.assertEqual(release["ref"], "refs/tags/v0.1.1")
        self.assertEqual(len(release["artifacts"]), 4)
        self.assertEqual(set(release["oci_platform_digests"]), {"linux/amd64", "linux/arm64"})
        self.assertEqual(len((dist / "SHA256SUMS").read_text().splitlines()), 4)
        publish.verify_bundle(self.args())

    def test_rejects_bundle_substitution_or_wrong_source(self):
        args = self.args()
        publish.assemble(args)
        provenance = args.out / "provenance.json"
        original = provenance.read_text()
        content = json.loads(original)
        content["commit"] = "b" * 40
        provenance.write_text(json.dumps(content))
        with self.assertRaises(ValueError):
            publish.verify_bundle(args)
        provenance.write_text(original)
        archive = next(args.out.glob("*.tar.gz"))
        archive.write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            publish.verify_bundle(args)

    def test_rejects_changed_native_archive(self):
        archive = self.inputs / f"native-{evidence.TARGETS[0]}-first" / "candidate.tar.gz"
        archive.write_bytes(b"tampered")
        with self.assertRaises((ValueError, OSError)):
            publish.assemble(self.args())

    def test_rejects_wrong_oci_source_or_digest(self):
        directory = self.inputs / "oci-amd64-second"
        for name, key, value in [("source.json", "commit", "b" * 40),
                                 ("source.json", "platform", "linux/arm64"),
                                 ("source.json", "attempt", "2"),
                                 ("build.json", "containerimage.digest", "sha256:" + "3" * 64),
                                 ("layers.json", "manifest", "sha256:" + "4" * 64)]:
            path = directory / name
            original = path.read_text()
            changed = json.loads(original)
            changed[key] = value
            path.write_text(json.dumps(changed))
            with self.subTest(key=key), self.assertRaises(ValueError):
                publish.assemble(self.args())
            path.write_text(original)


if __name__ == "__main__":
    unittest.main()
