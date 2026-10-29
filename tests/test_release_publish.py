"""Publisher guards use only synthetic candidates and temporary files."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
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

    def test_publisher_stages_draft_before_writing_image(self):
        text = (ROOT / ".github/workflows/publish-release.yml").read_text()
        self.assertRegex(text, r"(?m)^  push-platform:\n    name:.*\n    needs: \[source, assemble, stage-release\]")
        self.assertNotIn('gh api "repos/$GITHUB_REPOSITORY/releases/tags/$RELEASE_TAG"', text)

    @unittest.skipUnless(shutil.which("jq"), "jq is required by the publisher workflow")
    def test_draft_lookup_rejects_wrong_or_ambiguous_releases(self):
        text = (ROOT / ".github/workflows/publish-release.yml").read_text()
        selector = re.search(r"'(add \| map\(select\(\.tag_name == \$tag\)\) \| if length == 1[^']+)'", text).group(1)
        guard = re.search(r"'(\.tag_name == \$tag and \.draft == true and \.immutable != true and \(\.id \| tostring\) == \$id)'", text).group(1)
        draft = {"tag_name": "v0.1.1", "draft": True, "immutable": False, "id": 123}
        def jq(program, value, release_id="123"):
            return subprocess.run(["jq", "-e", "--arg", "tag", "v0.1.1", "--arg", "id", release_id, program],
                                  input=json.dumps(value), text=True, capture_output=True)
        selected = jq(selector, [[draft]])
        self.assertEqual(selected.returncode, 0)
        self.assertEqual(jq(guard, json.loads(selected.stdout)).returncode, 0)
        self.assertNotEqual(jq(guard, draft, "999").returncode, 0)
        self.assertNotEqual(jq(guard, {**draft, "draft": False}).returncode, 0)
        self.assertNotEqual(jq(selector, [[]]).returncode, 0)
        self.assertNotEqual(jq(selector, [[draft, draft]]).returncode, 0)

    def test_pr_preflight_remains_read_only(self):
        text = (ROOT / ".github/workflows/release-preflight.yml").read_text()
        self.assertEqual(set(self.events(text)), {"pull_request", "workflow_dispatch", "workflow_call"})
        for prohibited in ("contents: write", "packages: write", "attestations: write", "id-token: write",
                           "gh release create", "push=true", "push: true", "type=registry"):
            with self.subTest(prohibited=prohibited):
                self.assertNotIn(prohibited, text)


@unittest.skipUnless(shutil.which("jq") and shutil.which("bash"), "publisher requires jq and bash")
class OciIndexSubjectTests(unittest.TestCase):
    """Exercise the workflow shell with a disposable, moving registry tag."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.good = "sha256:" + "a" * 64
        self.bad = "sha256:" + "b" * 64
        self.amd = "sha256:" + "1" * 64
        self.arm = "sha256:" + "2" * 64
        self.state = self.root / "registry.json"
        self.output = self.root / "github-output"
        self.output.write_text("")
        self.state.write_text(json.dumps({"tag": self.good, "move_after_tag_read": False}))
        docker = self.root / "docker"
        docker.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
            import json, os, pathlib, sys
            state_file = pathlib.Path(os.environ["FAKE_REGISTRY"])
            state = json.loads(state_file.read_text())
            args = sys.argv[1:]
            if args[:3] == ["buildx", "imagetools", "create"]:
                sys.exit(0)
            if args[:3] != ["buildx", "imagetools", "inspect"]:
                sys.exit(2)
            ref = args[3]
            tag = os.environ["IMAGE"] + ":" + os.environ["RELEASE_TAG"]
            if ref == tag:
                digest = state["tag"]
                if state["move_after_tag_read"]:
                    state["tag"] = os.environ["BAD_INDEX"]
                    state["move_after_tag_read"] = False
                    state_file.write_text(json.dumps(state))
            elif ref.startswith(os.environ["IMAGE"] + "@"):
                digest = ref.split("@", 1)[1]
            else:
                sys.exit(2)
            if "--raw" in args:
                amd = os.environ["AMD_DIGEST"] if digest == os.environ["GOOD_INDEX"] else os.environ["BAD_INDEX"]
                print(json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
                    "manifests": [{"platform": {"os": "linux", "architecture": "amd64"}, "digest": amd},
                                  {"platform": {"os": "linux", "architecture": "arm64"}, "digest": os.environ["ARM_DIGEST"]}]}))
            elif "--format" in args:
                print(json.dumps({"digest": digest}))
            else:
                sys.exit(2)
            '''))
        docker.chmod(0o755)
        gh = self.root / "gh"
        gh.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
            import os, sys
            expected = "oci://" + os.environ["IMAGE"] + "@" + os.environ["GOOD_INDEX"]
            sys.exit(0 if sys.argv[1:3] == ["attestation", "verify"] and sys.argv[3] == expected else 2)
            '''))
        gh.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                        IMAGE="example.invalid/memoryd", RELEASE_TAG="v0.1.1",
                        FAKE_REGISTRY=str(self.state), GITHUB_OUTPUT=str(self.output),
                        GOOD_INDEX=self.good, BAD_INDEX=self.bad,
                        AMD_DIGEST=self.amd, ARM_DIGEST=self.arm,
                        amd=self.amd, arm=self.arm, GITHUB_REPOSITORY="example/memoryd",
                        APPROVED_COMMIT=COMMIT, GITHUB_REF="refs/tags/v0.1.1")
        self.workflow = (ROOT / ".github/workflows/publish-release.yml").read_text()

    def run_workflow_part(self, start, end, command_start):
        part = self.workflow.split(start, 1)[1].split(end, 1)[0]
        shell = textwrap.dedent(part.split("        run: |\n", 1)[1])
        shell = "set -euo pipefail\n" + shell[shell.index(command_start):]
        return subprocess.run(["bash", "-c", shell], env=self.env, text=True, capture_output=True)

    def select_index(self):
        return self.run_workflow_part(
            "      - name: Create a versioned index only from verified platform digests\n",
            "      - uses: actions/attest@", "docker buildx imagetools create")

    def check_version_tag(self):
        self.env["DIGEST"] = self.good
        return self.run_workflow_part(
            "      - name: Verify OCI attestation and unchanged version digest\n",
            "      - name: Verify public image availability and isolated foreground service\n",
            "gh attestation verify")

    def test_unchanged_tag_selects_and_rechecks_the_verified_index(self):
        result = self.select_index()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f"digest={self.good}\n")
        self.assertEqual(self.check_version_tag().returncode, 0)

    def test_tag_movement_between_reads_cannot_select_unverified_index(self):
        self.state.write_text(json.dumps({"tag": self.good, "move_after_tag_read": True}))
        result = self.select_index()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f"digest={self.good}\n")
        self.assertNotEqual(self.check_version_tag().returncode, 0)

    def test_tag_movement_after_selection_fails_the_version_recheck(self):
        result = self.select_index()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f"digest={self.good}\n")
        self.state.write_text(json.dumps({"tag": self.bad, "move_after_tag_read": False}))
        self.assertNotEqual(self.check_version_tag().returncode, 0)

    def test_unverified_index_selected_before_validation_is_rejected(self):
        self.state.write_text(json.dumps({"tag": self.bad, "move_after_tag_read": False}))
        self.assertNotEqual(self.select_index().returncode, 0)
        self.assertEqual(self.output.read_text(), "")


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
