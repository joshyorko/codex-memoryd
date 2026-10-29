"""OCI diagnostics read owned candidate archives and detect content changes."""

import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "inspect_oci_layers.py"


def blob(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def write_candidate(path, content):
    inner = io.BytesIO()
    with gzip.GzipFile(fileobj=inner, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as layer:
            member = tarfile.TarInfo("usr/bin/sample")
            member.size = len(content)
            layer.addfile(member, io.BytesIO(content))
    layer_bytes = inner.getvalue()
    config = json.dumps({"rootfs": {"diff_ids": ["sha256:" + "0" * 64]}}).encode()
    manifest = json.dumps({"schemaVersion": 2, "config": {"digest": blob(config)},
                           "layers": [{"digest": blob(layer_bytes)}]}).encode()
    index = json.dumps({"schemaVersion": 2, "manifests": [{"digest": blob(manifest)}]}).encode()
    with tarfile.open(path, "w") as archive:
        for name, data in (("index.json", index),
                           ("blobs/sha256/" + blob(manifest)[7:], manifest),
                           ("blobs/sha256/" + blob(config)[7:], config),
                           ("blobs/sha256/" + blob(layer_bytes)[7:], layer_bytes)):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    return blob(manifest)


class OciLayerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def run_tool(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)],
                              capture_output=True, text=True, check=False)

    def test_summary_checks_manifest_and_records_file_digest(self):
        archive = self.root / "candidate.tar"
        digest = write_candidate(archive, b"first")
        result = self.run_tool("summarize", "--archive", archive, "--manifest", digest)
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(summary["layers"][0]["files"]["usr/bin/sample"]["sha256"],
                         hashlib.sha256(b"first").hexdigest())

    def test_summary_rejects_wrong_manifest(self):
        archive = self.root / "candidate.tar"
        write_candidate(archive, b"first")
        result = self.run_tool("summarize", "--archive", archive,
                               "--manifest", "sha256:" + "f" * 64)
        self.assertNotEqual(result.returncode, 0)

    def test_diff_identifies_changed_file(self):
        summaries = []
        for index, content in enumerate((b"first", b"second")):
            archive = self.root / f"candidate-{index}.tar"
            digest = write_candidate(archive, content)
            summary = self.root / f"summary-{index}.json"
            result = self.run_tool("summarize", "--archive", archive, "--manifest", digest)
            self.assertEqual(result.returncode, 0, result.stderr)
            summary.write_text(result.stdout)
            summaries.append(summary)
        result = self.run_tool("diff", *summaries)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("usr/bin/sample", result.stdout)


if __name__ == "__main__":
    unittest.main()
