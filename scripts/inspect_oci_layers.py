#!/usr/bin/env python3
"""Summarize non-release OCI images without extracting or publishing them."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def read_member(archive, name, limit):
    member = archive.getmember(name)
    if not member.isfile() or member.size > limit:
        raise ValueError(f"invalid OCI member: {name}")
    return archive.extractfile(member).read()


def read_blob(archive, blob_digest, limit):
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", blob_digest):
        raise ValueError("invalid OCI digest")
    data = read_member(archive, "blobs/sha256/" + blob_digest[7:], limit)
    if digest(data) != blob_digest:
        raise ValueError("OCI blob digest mismatch")
    return data


def summarize(args):
    with tarfile.open(args.archive, "r:") as archive:
        index = json.loads(read_member(archive, "index.json", 1024 * 1024))
        manifests = index.get("manifests", [])
        if len(manifests) != 1 or manifests[0].get("digest") != args.manifest:
            raise ValueError("OCI index does not identify the expected manifest")
        manifest = json.loads(read_blob(archive, args.manifest, 1024 * 1024))
        config_digest = manifest["config"]["digest"]
        config = json.loads(read_blob(archive, config_digest, 1024 * 1024))
        result = {"manifest": args.manifest, "config": config_digest,
                  "diff_ids": config["rootfs"]["diff_ids"], "layers": []}
        for descriptor in manifest["layers"]:
            layer_digest = descriptor["digest"]
            compressed = read_blob(archive, layer_digest, 512 * 1024 * 1024)
            files = {}
            with tarfile.open(fileobj=io.BytesIO(compressed), mode="r:*") as layer:
                for entry in layer:
                    if entry.name in files:
                        raise ValueError(f"duplicate layer entry: {entry.name}")
                    record = {"type": entry.type.decode("latin-1"), "size": entry.size,
                              "mode": entry.mode, "uid": entry.uid, "gid": entry.gid,
                              "mtime": entry.mtime, "link": entry.linkname,
                              "pax": entry.pax_headers}
                    if entry.isfile():
                        hasher = hashlib.sha256()
                        with layer.extractfile(entry) as file:
                            for chunk in iter(lambda: file.read(1024 * 1024), b""):
                                hasher.update(chunk)
                        record["sha256"] = hasher.hexdigest()
                    files[entry.name] = record
            result["layers"].append({"digest": layer_digest, "files": files})
    print(json.dumps(result, sort_keys=True))


def compare(args):
    first = json.loads(args.first.read_text())
    second = json.loads(args.second.read_text())
    if first["manifest"] == second["manifest"]:
        print("OCI candidate manifests match")
        return 0
    print("OCI candidate manifests differ")
    for index in range(max(len(first["layers"]), len(second["layers"]))):
        a = first["layers"][index] if index < len(first["layers"]) else {"files": {}}
        b = second["layers"][index] if index < len(second["layers"]) else {"files": {}}
        if a.get("digest") == b.get("digest"):
            continue
        changed = [name for name in sorted(set(a["files"]) | set(b["files"]))
                   if a["files"].get(name) != b["files"].get(name)]
        print(f"layer {index}: {len(changed)} changed entries")
        for name in changed[:100]:
            print(f"  {name}")
    return 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    scan = commands.add_parser("summarize")
    scan.add_argument("--archive", type=Path, required=True)
    scan.add_argument("--manifest", required=True)
    diff = commands.add_parser("diff")
    diff.add_argument("first", type=Path)
    diff.add_argument("second", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "summarize":
            summarize(args)
            return 0
        return compare(args)
    except (KeyError, OSError, ValueError, TypeError, tarfile.TarError) as error:
        print(f"OCI candidate evidence: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
