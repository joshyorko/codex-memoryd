#!/usr/bin/env python3
"""Run isolated Linux checks with orphan reaping and bounded owned cleanup."""
import argparse
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

CLEANUP_GRACE_SECONDS = 2.0
KILL_WAIT_SECONDS = 1.0


def adopted_children(main_pid):
    """Only direct children of this launcher, excluding the Popen owner."""
    children = Path(f"/proc/self/task/{os.getpid()}/children").read_text().split()
    return [int(pid) for pid in children if int(pid) != main_pid]


def reap_adopted(main_pid):
    for pid in adopted_children(main_pid):
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
    return adopted_children(main_pid)


def send_signal(pid, signum, process_group=False):
    try:
        if process_group:
            os.killpg(pid, signum)
        else:
            os.kill(pid, signum)
    except ProcessLookupError:
        pass


def run(command):
    if sys.platform != "linux":
        raise RuntimeError("child-subreaper checks require Linux")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER failed")

    pending_signals = []
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, lambda received, _frame: pending_signals.append(received))
    child = subprocess.Popen(command, start_new_session=True)
    cleanup_started = None
    signaled_children = set()
    while True:
        # Popen alone owns the main command's wait status. Never waitpid(-1),
        # which could steal its exit code while reaping adopted descendants.
        returncode = child.poll()
        children = reap_adopted(child.pid)
        now = time.monotonic()
        if pending_signals:
            cleanup_started = cleanup_started or now
            for signum in pending_signals:
                if returncode is None:
                    send_signal(child.pid, signum, process_group=True)
                for pid in children:
                    send_signal(pid, signum)
            pending_signals.clear()
        if returncode is not None:
            if not children:
                return returncode if returncode >= 0 else 128 - returncode
            cleanup_started = cleanup_started or now

        if cleanup_started is not None:
            elapsed = now - cleanup_started
            signum = signal.SIGKILL if elapsed >= CLEANUP_GRACE_SECONDS else signal.SIGTERM
            if signum == signal.SIGKILL and returncode is None:
                send_signal(child.pid, signum, process_group=True)
            for pid in children:
                if signum == signal.SIGKILL or pid not in signaled_children:
                    send_signal(pid, signum)
                    signaled_children.add(pid)
            if elapsed >= CLEANUP_GRACE_SECONDS + KILL_WAIT_SECONDS:
                returncode = child.poll()
                remaining = reap_adopted(child.pid)
                if returncode is not None and not remaining:
                    return returncode if returncode >= 0 else 128 - returncode
                print("child reaper: owned fixture cleanup exceeded its deadline", file=sys.stderr)
                if returncode is not None and returncode != 0:
                    return returncode if returncode > 0 else 128 - returncode
                return 1
        time.sleep(0.01)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("an isolated check command is required")
    try:
        return run(command)
    except (OSError, RuntimeError) as error:
        print(f"child reaper: {error}", file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main())
