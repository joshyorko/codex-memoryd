"""The release remaps must survive different checkout and Cargo paths."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "release-native-build-env.sh"


class NativeBuildEnvironmentTests(unittest.TestCase):
    def test_paths_with_spaces_are_preserved_as_two_cargo_flags(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "source checkout"
            cargo = Path(tmp) / "cargo home"
            root.mkdir()
            cargo.mkdir()
            env = {**os.environ, "CARGO_HOME": str(cargo), "RUSTFLAGS": "--cfg=unwanted"}
            result = subprocess.run(
                ["bash", "-c", 'source "$1" && printf "%s" "$CARGO_ENCODED_RUSTFLAGS"', "bash", str(SCRIPT)],
                cwd=root, env=env, capture_output=True, check=True,
            )
            self.assertEqual(result.stdout.decode().split("\x1f"), [
                f"--remap-path-prefix={root}=/usr/src/codex-memoryd",
                f"--remap-path-prefix={cargo}=/usr/src/cargo",
            ])

    def test_ambiguous_cargo_home_fails_before_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            for cargo in ("relative/path", "/tmp/a=b", "/tmp/a\x1fb"):
                result = subprocess.run(
                    ["bash", "-c", 'source "$1"', "bash", str(SCRIPT)],
                    cwd=tmp, env={**os.environ, "CARGO_HOME": cargo}, capture_output=True,
                )
                with self.subTest(cargo=cargo):
                    self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
