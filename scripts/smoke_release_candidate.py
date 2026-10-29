#!/usr/bin/env python3
"""Exercise a candidate binary only against owned processes and temporary state."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def run(binary, environment, *arguments, success=True):
    result = subprocess.run(
        [str(binary), *arguments], env=environment, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20, check=False,
    )
    if (result.returncode == 0) != success:
        raise RuntimeError(f"{arguments} returned {result.returncode}: {result.stderr}")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="memoryd-release-smoke-") as directory:
        root = Path(directory)
        runtime = root / "runtime"
        runtime.mkdir()
        user_home = root / "home"
        user_home.mkdir()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        bind = f"127.0.0.1:{port}"
        environment = {
            "PATH": os.defpath,
            "HOME": str(user_home),
            "CODEX_MEMORYD_HOME": str(runtime),
            "CODEX_MEMORYD_BIND": bind,
            "CODEX_MEMORYD_PORT": str(port),
        }
        if run(binary, environment, "--version").strip() != f"codex-memoryd {args.version}":
            raise RuntimeError("candidate version mismatch")

        pid_path = runtime / "codex-memoryd.pid"
        with subprocess.Popen(["/bin/sleep", "60"]) as fixture:
            try:
                pid_path.write_text(str(fixture.pid))
                report = json.loads(run(binary, environment, "--runtime", "native", "status"))
                if report.get("process") != "unverified" or report.get("pid") is not None:
                    raise RuntimeError("legacy PID was incorrectly accepted")
                for action in ("down", "restart", "up"):
                    run(binary, environment, "--runtime", "native", action, success=False)
                    if fixture.poll() is not None or pid_path.read_text() != str(fixture.pid):
                        raise RuntimeError(f"{action} changed unrelated process or state")
            finally:
                if fixture.poll() is None:
                    fixture.terminate()
                fixture.wait(timeout=5)
        pid_path.unlink()

        run(binary, environment, "init", "--bind", bind)
        if sys.platform.startswith("linux"):
            try:
                for action in ("up", "restart"):
                    report = json.loads(run(binary, environment, "--runtime", "native", action))
                    if report.get("process") != "running" or report.get("health") != "ok":
                        raise RuntimeError(f"{action} did not start a healthy candidate")
                report = json.loads(run(binary, environment, "--runtime", "native", "down"))
                if report.get("process") != "stopped":
                    raise RuntimeError("down did not stop the candidate")
            finally:
                # This is an isolated fixture runtime; the Linux caller also
                # owns and reaps any remaining descendants.
                run(binary, environment, "--runtime", "native", "down")
        else:
            for action in ("up", "restart", "down"):
                run(binary, environment, "--runtime", "native", action, success=False)
            database = runtime / "memory.db"
            with subprocess.Popen(
                [str(binary), "serve", "--bind", bind, "--db", str(database)],
                env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            ) as daemon:
                try:
                    deadline = time.monotonic() + 20
                    while time.monotonic() < deadline:
                        if daemon.poll() is not None:
                            raise RuntimeError("foreground daemon exited before health check")
                        try:
                            with urllib.request.urlopen(f"http://{bind}/healthz", timeout=1) as response:
                                if json.load(response).get("ok") is True:
                                    break
                        except (OSError, urllib.error.URLError):
                            time.sleep(0.1)
                    else:
                        raise RuntimeError("foreground daemon health check timed out")
                finally:
                    if daemon.poll() is None:
                        daemon.terminate()
                    daemon.wait(timeout=5)
            if not database.is_file() or database.stat().st_size == 0:
                raise RuntimeError("foreground database was not created")
        if not (runtime / "memory.db").is_file():
            raise RuntimeError("initialized database was not preserved")
    print("isolated candidate lifecycle passed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"release smoke: {error}", file=sys.stderr)
        sys.exit(1)
