"""Failure diagnostics identify differences without relaxing candidate checks."""

import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "inspect_native_mismatch.py"
SPEC = importlib.util.spec_from_file_location("inspect_native_mismatch", SCRIPT)
DIAGNOSTIC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DIAGNOSTIC)


def macho():
    uuid = struct.pack("<II16s", 0x1B, 24, b"u" * 16)
    section = struct.pack("<16s16sQQIIIIIIII", b"__text", b"__TEXT", 0, 16, 512, 0, 0, 0, 0, 0, 0, 0)
    segment = struct.pack("<II16sQQQQiiII", 0x19, 152, b"__TEXT", 0, 1024, 0, 1024, 7, 5, 1, 0) + section
    signature = struct.pack("<IIII", 0x1D, 16, 900, 16)
    commands = uuid + segment + signature
    header = struct.pack("<IiiIIIII", 0xFEEDFACF, 0, 0, 2, 3, len(commands), 0, 0)
    return (header + commands).ljust(1024, b"\0")


class NativeMismatchTests(unittest.TestCase):
    def test_reports_section_and_signature_changes(self):
        first = macho()
        second = bytearray(first)
        second[515] = 1
        second[903] = 2
        report = DIAGNOSTIC.inspect(first, bytes(second))
        self.assertEqual(report["differing_bytes"], 2)
        self.assertEqual(report["first_layout"]["uuid"], (b"u" * 16).hex())
        differences = {item["name"]: item["differing_bytes"] for item in report["first_layout_region_differences"]}
        self.assertEqual(differences["section:__TEXT,__text"], 1)
        self.assertEqual(differences["code-signature"], 1)

    def test_differing_lengths_and_non_macho_are_reported(self):
        report = DIAGNOSTIC.inspect(b"abc", b"abXYZ")
        self.assertEqual(report["differing_bytes"], 3)
        self.assertEqual(report["first_layout"]["format"], "other")

    def test_malformed_commands_fail_closed(self):
        binary = bytearray(macho())
        struct.pack_into("<I", binary, 36, len(binary))
        with self.assertRaises(ValueError):
            DIAGNOSTIC.macho_layout(binary)

    def test_archive_must_have_exactly_the_expected_regular_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with tarfile.open(root / "candidate.tar.gz", "w:gz") as archive:
                item = tarfile.TarInfo("../codex-memoryd")
                item.size = 1
                archive.addfile(item, io.BytesIO(b"x"))
            with self.assertRaises(ValueError):
                DIAGNOSTIC.read_binary(root)


if __name__ == "__main__":
    unittest.main()
