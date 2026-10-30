#!/usr/bin/env python3
"""Diagnose candidate differences without changing the reproducibility gate."""

import argparse
import json
from pathlib import Path
import struct
import sys
import tarfile


MAX_BINARY_SIZE = 512 * 1024 * 1024


def read_binary(candidate):
    with tarfile.open(candidate / "candidate.tar.gz", "r:gz") as archive:
        members = archive.getmembers()
        if (len(members) != 1 or not members[0].isfile()
                or members[0].name != "codex-memoryd"
                or not 0 < members[0].size <= MAX_BINARY_SIZE):
            raise ValueError("invalid candidate archive")
        return archive.extractfile(members[0]).read()


def macho_layout(binary):
    """Describe the on-disk ranges of a little-endian 64-bit Mach-O file."""
    if len(binary) < 32 or binary[:4] != b"\xcf\xfa\xed\xfe":
        return {"format": "other", "regions": []}
    _, _, _, _, ncmds, commands_size, _, _ = struct.unpack_from("<IiiIIIII", binary)
    commands_end = 32 + commands_size
    if commands_end > len(binary) or ncmds > commands_size // 8:
        raise ValueError("invalid Mach-O load command bounds")
    layout = {"format": "macho64", "uuid": None, "regions": []}

    def region(name, offset, size):
        if offset < 0 or size < 0 or offset + size > len(binary):
            raise ValueError(f"invalid Mach-O {name} bounds")
        layout["regions"].append({"name": name, "offset": offset, "size": size})

    region("header-and-load-commands", 0, commands_end)
    cursor = 32
    for _ in range(ncmds):
        if cursor + 8 > commands_end:
            raise ValueError("truncated Mach-O command")
        command, command_size = struct.unpack_from("<II", binary, cursor)
        if command_size < 8 or cursor + command_size > commands_end:
            raise ValueError("invalid Mach-O command size")
        if command == 0x1B:  # LC_UUID
            if command_size < 24:
                raise ValueError("truncated Mach-O UUID")
            layout["uuid"] = binary[cursor + 8:cursor + 24].hex()
        elif command == 0x1D:  # LC_CODE_SIGNATURE
            if command_size < 16:
                raise ValueError("truncated Mach-O signature command")
            offset, size = struct.unpack_from("<II", binary, cursor + 8)
            region("code-signature", offset, size)
        elif command == 0x19:  # LC_SEGMENT_64
            if command_size < 72:
                raise ValueError("truncated Mach-O segment")
            values = struct.unpack_from("<II16sQQQQiiII", binary, cursor)
            segment = values[2].split(b"\0", 1)[0].decode("ascii", errors="replace")
            offset, size, sections = values[5], values[6], values[9]
            region(f"segment:{segment}", offset, size)
            if sections > (command_size - 72) // 80:
                raise ValueError("invalid Mach-O section count")
            for index in range(sections):
                section = struct.unpack_from("<16s16sQQIIIIIIII", binary, cursor + 72 + index * 80)
                name = section[0].split(b"\0", 1)[0].decode("ascii", errors="replace")
                section_size, section_offset, flags = section[3], section[4], section[8]
                if flags & 0xFF not in (0x01, 0x0C, 0x12):  # zero-fill sections have no file bytes
                    region(f"section:{segment},{name}", section_offset, section_size)
        cursor += command_size
    if cursor != commands_end:
        raise ValueError("unexpected Mach-O load command padding")
    return layout


def inspect(first, second):
    first_layout, second_layout = macho_layout(first), macho_layout(second)
    common = min(len(first), len(second))
    ranges = []
    differing = 0
    # Skip equal blocks quickly while keeping output bounded even for many changes.
    for start in range(0, common, 4096):
        left, right = first[start:start + 4096], second[start:start + 4096]
        if left == right:
            continue
        begin = None
        for index, (a, b) in enumerate(zip(left, right)):
            if a != b:
                differing += 1
                if begin is None:
                    begin = start + index
            elif begin is not None:
                if len(ranges) < 32:
                    ranges.append([begin, start + index])
                begin = None
        if begin is not None and len(ranges) < 32:
            ranges.append([begin, start + len(left)])
    differing += abs(len(first) - len(second))
    region_differences = []
    for region in first_layout["regions"]:
        start, end = region["offset"], region["offset"] + region["size"]
        end_common = min(end, common)
        count = sum(a != b for a, b in zip(first[start:end_common], second[start:end_common]))
        count += max(0, end - max(start, common))
        if count:
            region_differences.append({**region, "differing_bytes": count})
    return {
        "first_size": len(first), "second_size": len(second),
        "differing_bytes": differing, "first_differing_ranges": ranges,
        "first_layout": first_layout, "second_layout": second_layout,
        "first_layout_region_differences": region_differences,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--first", type=Path, required=True)
    parser.add_argument("--second", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(inspect(read_binary(args.first), read_binary(args.second)), indent=2))
    except (OSError, ValueError, struct.error, tarfile.TarError) as error:
        print(f"candidate mismatch diagnostic: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
