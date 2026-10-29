#!/usr/bin/env python3
"""Validate an explicit release source and assemble matched CI candidates.

This script never contacts GitHub or publishes artifacts. Its inputs must come
from the same workflow run, and the caller must independently verify the tag
and ancestry before any external writes.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
import release_evidence

REPOSITORY = "joshyorko/codex-memoryd"
ARCHES = ("amd64", "arm64")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
SHA = re.compile(r"[0-9a-f]{40}\Z")


def validate_source(args):
    if args.event != "workflow_dispatch":
        raise ValueError("release requires a manual workflow dispatch")
    if not args.tag.startswith("v") or not VERSION.fullmatch(args.tag[1:]):
        raise ValueError("release tag must be v followed by a stable version")
    if args.ref != f"refs/tags/{args.tag}":
        raise ValueError("selected ref does not match the approved tag")
    if not SHA.fullmatch(args.expected_commit):
        raise ValueError("approved commit must be a full lowercase SHA")
    if args.event_commit != args.expected_commit or args.checkout_commit != args.expected_commit:
        raise ValueError("event, checkout, and approved commits must match")
    if args.version != args.tag[1:]:
        raise ValueError("package version does not match the approved tag")
    return args.version


def validate_remote_tag(output, tag, expected_commit):
    if not tag.startswith("v") or not VERSION.fullmatch(tag[1:]) or not SHA.fullmatch(expected_commit):
        raise ValueError("invalid remote tag identity")
    ref = f"refs/tags/{tag}"
    refs = {}
    for line in output.splitlines():
        parts = line.split("\t")
        if len(parts) != 2 or not SHA.fullmatch(parts[0]) or parts[1] not in (ref, ref + "^{}"):
            raise ValueError("invalid remote tag response")
        if parts[1] in refs:
            raise ValueError("duplicate remote tag response")
        refs[parts[1]] = parts[0]
    if ref not in refs or refs.get(ref + "^{}", refs[ref]) != expected_commit:
        raise ValueError("remote tag no longer resolves to the approved commit")


def read_json(path, max_size=64 * 1024 * 1024):
    if path.stat().st_size > max_size:
        raise ValueError(f"evidence exceeds size limit: {path}")
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        raise ValueError(f"evidence must be an object: {path}")
    return data


def verify_oci_pair(root, arch, args, lock_hash):
    sources, builds, layers = [], [], []
    for slot in ("first", "second"):
        directory = root / f"oci-{arch}-{slot}"
        source = read_json(directory / "source.json", 65536)
        expected = {"commit": args.commit, "version": args.version,
                    "platform": f"linux/{arch}", "slot": slot, "run": args.run_id,
                    "attempt": args.run_attempt, "lock": lock_hash,
                    "kind": "unsigned-nonrelease-candidate"}
        if any(source.get(key) != value for key, value in expected.items()):
            raise ValueError(f"OCI {arch} {slot} source identity mismatch")
        for key in ("buildx", "buildkit"):
            if not isinstance(source.get(key), str) or not source[key].strip():
                raise ValueError(f"OCI {arch} {slot} missing {key}")
        build = read_json(directory / "build.json", 4 * 1024 * 1024)
        manifest = build.get("containerimage.digest")
        if not isinstance(manifest, str) or not DIGEST.fullmatch(manifest):
            raise ValueError(f"OCI {arch} {slot} invalid digest")
        if not isinstance(build.get("buildx.build.provenance"), dict) or not build["buildx.build.provenance"]:
            raise ValueError(f"OCI {arch} {slot} missing build metadata")
        layer = read_json(directory / "layers.json")
        if layer.get("manifest") != manifest:
            raise ValueError(f"OCI {arch} {slot} manifest evidence mismatch")
        sources.append(source)
        builds.append(build)
        layers.append(layer)
    if any(sources[0][key] != sources[1][key] for key in ("buildx", "buildkit")):
        raise ValueError(f"OCI {arch} build tools differ")
    if builds[0]["containerimage.digest"] != builds[1]["containerimage.digest"]:
        raise ValueError(f"OCI {arch} independent manifests differ")
    if layers[0] != layers[1]:
        raise ValueError(f"OCI {arch} independent layer evidence differs")
    return builds[0]["containerimage.digest"], {
        "buildx": sources[0]["buildx"], "buildkit": sources[0]["buildkit"]}


def verify_candidates(args):
    # identity validates SHA, version, run, and attempt; each target has its
    # own record and only the matching first/second pair can be selected.
    verified = []
    for target in release_evidence.TARGETS:
        expected = release_evidence.identity(argparse.Namespace(
            commit=args.commit, version=args.version, target=target, lockfile=args.lockfile,
            run_id=args.run_id, run_attempt=args.run_attempt))
        first = args.candidates / f"native-{target}-first"
        second = args.candidates / f"native-{target}-second"
        a = release_evidence.verify_candidate(first, expected, "first")
        b = release_evidence.verify_candidate(second, expected, "second")
        for key in ("rustc", "cargo", "binary_sha256", "binary_size", "tar_sha256",
                    "archive_sha256", "archive_size", "python_version", "zlib_build", "zlib_runtime"):
            if a[key] != b[key]:
                raise ValueError(f"native {target} independent {key} mismatch")
        verified.append((target, first / release_evidence.ARCHIVE, a))
    lock_hash = hashlib.sha256(args.lockfile.read_bytes()).hexdigest()
    oci = {f"linux/{arch}": verify_oci_pair(args.candidates, arch, args, lock_hash)
           for arch in ARCHES}
    return verified, oci, lock_hash


def assemble(args):
    if args.ref != f"refs/tags/v{args.version}":
        raise ValueError("release evidence ref and version mismatch")
    verified, oci, lock_hash = verify_candidates(args)
    args.out.mkdir(parents=True, exist_ok=False)
    records = []
    for target, archive, data in verified:
        name = f"codex-memoryd-v{args.version}-{target}.tar.gz"
        shutil.copyfile(archive, args.out / name)
        records.append({"name": name, "target": target, "sha256": data["archive_sha256"],
                        "size_bytes": data["archive_size"]})
    (args.out / "SHA256SUMS").write_text("".join(f"{a['sha256']}  {a['name']}\n" for a in records))
    provenance = {"schema_version": 2, "repository": REPOSITORY, "commit": args.commit,
                  "ref": args.ref, "package_version": args.version, "workflow_run": int(args.run_id),
                  "workflow_attempt": int(args.run_attempt), "lock_sha256": lock_hash,
                  "artifacts": records,
                  "oci_platform_digests": {key: value[0] for key, value in oci.items()},
                  "oci_build_tools": {key: value[1] for key, value in oci.items()},
                  "reproducibility": "two independent unsigned candidates matched"}
    (args.out / "provenance.json").write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n")


def verify_bundle(args):
    expected_identity = release_evidence.identity(argparse.Namespace(
        commit=args.commit, version=args.version, target=release_evidence.TARGETS[0],
        lockfile=args.lockfile, run_id=args.run_id, run_attempt=args.run_attempt))
    if args.ref != f"refs/tags/v{args.version}":
        raise ValueError("release bundle ref and version mismatch")
    names = [f"codex-memoryd-v{args.version}-{target}.tar.gz" for target in release_evidence.TARGETS]
    if {p.name for p in args.out.iterdir()} != set(names + ["SHA256SUMS", "provenance.json"]):
        raise ValueError("release bundle asset set mismatch")
    for path in args.out.iterdir():
        if path.is_symlink() or not path.is_file():
            raise ValueError("release bundle contains a non-regular file")
    provenance = read_json(args.out / "provenance.json", 65536)
    identity = {"schema_version": 2, "repository": REPOSITORY, "commit": args.commit,
                "ref": args.ref, "package_version": args.version, "workflow_run": int(args.run_id),
                "workflow_attempt": int(args.run_attempt), "lock_sha256": expected_identity["lock_sha256"],
                "reproducibility": "two independent unsigned candidates matched"}
    if any(provenance.get(key) != value for key, value in identity.items()):
        raise ValueError("release bundle source identity mismatch")
    records = provenance.get("artifacts")
    if not isinstance(records, list) or len(records) != len(names):
        raise ValueError("release bundle records mismatch")
    checksum_lines = []
    for target, name, record in zip(release_evidence.TARGETS, names, records):
        if not isinstance(record, dict) or record.get("name") != name or record.get("target") != target:
            raise ValueError("release bundle artifact identity mismatch")
        path = args.out / name
        if path.stat().st_size <= 0 or path.stat().st_size > 512 * 1024 * 1024:
            raise ValueError("release bundle archive size is invalid")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if record.get("sha256") != digest or record.get("size_bytes") != path.stat().st_size:
            raise ValueError("release bundle archive does not match its evidence")
        checksum_lines.append(f"{digest}  {name}\n")
    if (args.out / "SHA256SUMS").read_text() != "".join(checksum_lines):
        raise ValueError("release bundle checksum list mismatch")
    platforms = {f"linux/{arch}" for arch in ARCHES}
    digests = provenance.get("oci_platform_digests")
    tools = provenance.get("oci_build_tools")
    if not isinstance(digests, dict) or set(digests) != platforms or not isinstance(tools, dict) or set(tools) != platforms:
        raise ValueError("release bundle OCI platforms mismatch")
    for platform in platforms:
        if not isinstance(digests[platform], str) or not DIGEST.fullmatch(digests[platform]):
            raise ValueError("release bundle OCI digest invalid")
        if not isinstance(tools[platform], dict) or any(
            not isinstance(tools[platform].get(key), str) or not tools[platform][key].strip()
            for key in ("buildx", "buildkit")
        ):
            raise ValueError("release bundle OCI build tools invalid")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    source = commands.add_parser("validate-source")
    for name in ("event", "ref", "tag", "expected-commit", "event-commit", "checkout-commit", "version"):
        source.add_argument("--" + name, required=True)
    remote = commands.add_parser("verify-remote-tag")
    remote.add_argument("--tag", required=True)
    remote.add_argument("--commit", required=True)
    bundle = commands.add_parser("assemble")
    check = commands.add_parser("verify-bundle")
    candidates = commands.add_parser("verify-candidates")
    for command in (bundle, check, candidates):
        for name in ("commit", "version", "run-id", "run-attempt"):
            command.add_argument("--" + name, required=True)
        command.add_argument("--lockfile", type=Path, required=True)
    for command in (bundle, check):
        command.add_argument("--ref", required=True)
        command.add_argument("--out", type=Path, required=True)
    for command in (bundle, candidates):
        command.add_argument("--candidates", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "validate-source":
            print(validate_source(args))
        elif args.command == "verify-remote-tag":
            if not args.tag.startswith("v") or not VERSION.fullmatch(args.tag[1:]):
                raise ValueError("invalid release tag")
            ref = f"refs/tags/{args.tag}"
            result = subprocess.run(["git", "ls-remote", "origin", ref, ref + "^{}"],
                                    capture_output=True, text=True, check=True, timeout=30)
            validate_remote_tag(result.stdout, args.tag, args.commit)
        elif args.command == "assemble":
            assemble(args)
        elif args.command == "verify-candidates":
            verify_candidates(args)
            print("All same-run native and OCI candidate evidence matched.")
        else:
            verify_bundle(args)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"release publisher: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
