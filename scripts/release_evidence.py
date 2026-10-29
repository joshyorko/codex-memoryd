#!/usr/bin/env python3
"""Create and compare unsigned, non-release build-candidate evidence."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)
ARCHIVE = "candidate.tar.gz"
EVIDENCE = "evidence.json"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def identity(args):
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        raise ValueError("commit must be a full lowercase Git SHA")
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", args.version):
        raise ValueError("version must be a stable x.y.z version")
    if not re.fullmatch(r"[1-9][0-9]*", args.run_id):
        raise ValueError("run-id must be a positive integer")
    if not re.fullmatch(r"[1-9][0-9]*", args.run_attempt):
        raise ValueError("run-attempt must be a positive integer")
    return {
        "commit": args.commit,
        "version": args.version,
        "target": args.target,
        "lock_sha256": digest(args.lockfile.read_bytes()),
        "run_id": args.run_id,
        "run_attempt": args.run_attempt,
    }


def record(args):
    expected = identity(args)
    if not args.rustc.strip() or not args.cargo.strip():
        raise ValueError("toolchain versions are required")
    binary = args.binary.read_bytes()
    if not binary:
        raise ValueError("candidate binary is empty")
    args.out.mkdir(parents=True, exist_ok=False)
    archive = args.out / ARCHIVE
    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as bundle:
                info = tarfile.TarInfo("codex-memoryd")
                info.size = len(binary)
                info.mode = 0o755
                info.mtime = info.uid = info.gid = 0
                bundle.addfile(info, io.BytesIO(binary))
    evidence = {
        "schema": 1,
        "kind": "unsigned-nonrelease-candidate",
        **expected,
        "slot": args.slot,
        "rustc": args.rustc,
        "cargo": args.cargo,
        "binary_sha256": digest(binary),
        "binary_size": len(binary),
        "archive_sha256": digest(archive.read_bytes()),
        "archive_size": archive.stat().st_size,
    }
    (args.out / EVIDENCE).write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n")


def verify_candidate(directory, expected, slot):
    if {p.name for p in directory.iterdir()} != {ARCHIVE, EVIDENCE}:
        raise ValueError("candidate must contain exactly the archive and evidence")
    evidence_path = directory / EVIDENCE
    if evidence_path.stat().st_size > 65536:
        raise ValueError("candidate evidence is too large")
    evidence = json.loads(evidence_path.read_text())
    for key, value in {**expected, "schema": 1, "kind": "unsigned-nonrelease-candidate", "slot": slot}.items():
        if evidence.get(key) != value:
            raise ValueError(f"candidate {key} mismatch")
    archive = (directory / ARCHIVE).read_bytes()
    if evidence.get("archive_sha256") != digest(archive) or evidence.get("archive_size") != len(archive):
        raise ValueError("candidate archive mismatch")
    with tarfile.open(directory / ARCHIVE, mode="r:gz") as bundle:
        members = bundle.getmembers()
        if len(members) != 1:
            raise ValueError("candidate archive must contain one file")
        member = members[0]
        if not member.isfile() or member.name != "codex-memoryd" or member.mode != 0o755:
            raise ValueError("invalid candidate archive member")
        if member.mtime != 0 or member.uid != 0 or member.gid != 0:
            raise ValueError("candidate archive metadata is not normalized")
        if member.size <= 0 or member.size > 512 * 1024 * 1024:
            raise ValueError("invalid candidate binary size")
        binary = bundle.extractfile(member).read()
    if evidence.get("binary_sha256") != digest(binary) or evidence.get("binary_size") != len(binary):
        raise ValueError("candidate binary mismatch")
    if not isinstance(evidence.get("rustc"), str) or not evidence["rustc"].strip():
        raise ValueError("missing rustc evidence")
    if not isinstance(evidence.get("cargo"), str) or not evidence["cargo"].strip():
        raise ValueError("missing cargo evidence")
    return evidence


def compare(args):
    expected = identity(args)
    first = verify_candidate(args.first, expected, "first")
    second = verify_candidate(args.second, expected, "second")
    for key in ("rustc", "cargo", "binary_sha256", "binary_size", "archive_sha256", "archive_size"):
        if first[key] != second[key]:
            raise ValueError(f"independent build {key} mismatch")
    print(json.dumps({"result": "match", "target": args.target, "commit": args.commit,
                      "version": args.version, "archive_sha256": first["archive_sha256"]}, sort_keys=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("record")
    check = commands.add_parser("compare")
    for command in (create, check):
        command.add_argument("--commit", required=True)
        command.add_argument("--version", required=True)
        command.add_argument("--target", choices=TARGETS, required=True)
        command.add_argument("--lockfile", type=Path, required=True)
        command.add_argument("--run-id", required=True)
        command.add_argument("--run-attempt", required=True)
    create.add_argument("--binary", type=Path, required=True)
    create.add_argument("--out", type=Path, required=True)
    create.add_argument("--slot", choices=("first", "second"), required=True)
    create.add_argument("--rustc", required=True)
    create.add_argument("--cargo", required=True)
    check.add_argument("--first", type=Path, required=True)
    check.add_argument("--second", type=Path, required=True)
    args = parser.parse_args()
    try:
        (record if args.command == "record" else compare)(args)
    except (OSError, ValueError, json.JSONDecodeError, tarfile.TarError) as error:
        print(f"release evidence: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
