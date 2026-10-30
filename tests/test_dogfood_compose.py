"""Project selection is exercised with a fake Docker executable only."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "dogfood-compose.sh"


class DogfoodComposeTests(unittest.TestCase):
    def test_default_and_explicit_project_use_the_same_selector(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake_docker = root / "docker"
            fake_docker.write_text('#!/bin/bash\nprintf "%s\\n" "$@" > "$CAPTURE_ARGS"\n')
            fake_docker.chmod(0o755)
            captured = root / "args"
            for project, expected in ((None, "codex-memoryd"), ("dogfood-isolated", "dogfood-isolated")):
                env = {**os.environ, "PATH": f"{root}:/bin:/usr/bin", "CAPTURE_ARGS": str(captured)}
                env.pop("DOGFOOD_COMPOSE_PROJECT", None)
                if project is not None:
                    env["DOGFOOD_COMPOSE_PROJECT"] = project
                for command in (["up", "-d", "--build"], ["down"]):
                    with self.subTest(project=project, command=command):
                        subprocess.run([str(SCRIPT), *command], env=env, check=True, capture_output=True)
                        self.assertEqual(captured.read_text().splitlines(),
                                         ["compose", "--project-name", expected, *command])


if __name__ == "__main__":
    unittest.main()
