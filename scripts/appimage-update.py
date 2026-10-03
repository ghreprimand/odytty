#!/usr/bin/env python3
"""Verify an AppImage update control file before release publication."""
import argparse
import hashlib
from pathlib import Path
import struct

UPDATE_INFORMATION = "gh-releases-zsync|ghreprimand|odytty|latest|odytty-x86_64.AppImage.zsync"
ALIAS = "odytty-x86_64.AppImage"


def update_information(image: Path) -> str:
    """Read the bounded .upd_info section without executing the AppImage."""
    size = image.stat().st_size
    with image.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01":
            raise ValueError("expected a little-endian ELF64 AppImage")
        section_offset = struct.unpack_from("<Q", header, 40)[0]
        entry_size, count, names_index = struct.unpack_from("<HHH", header, 58)
        if entry_size != 64 or not 0 < count <= 4096 or names_index >= count:
            raise ValueError("invalid ELF section table")
        if section_offset + count * entry_size > size:
            raise ValueError("ELF section table is outside the image")
        stream.seek(section_offset)
        sections = [struct.unpack("<IIQQQQIIQQ", stream.read(64)) for _ in range(count)]
        names_section = sections[names_index]
        offset, length = names_section[4:6]
        if length > 65536 or offset + length > size:
            raise ValueError("invalid ELF section names")
        stream.seek(offset)
        names = stream.read(length)
        matches = []
        for section in sections:
            index = section[0]
            if index < len(names) and names[index:].split(b"\0", 1)[0] == b".upd_info":
                matches.append(section)
        if len(matches) != 1:
            raise ValueError("expected exactly one .upd_info section")
        offset, length = matches[0][4:6]
        if not 0 < length <= 4096 or offset + length > size:
            raise ValueError("invalid update information section")
        stream.seek(offset)
        return stream.read(length).rstrip(b"\0").decode("ascii")


def digest(image: Path) -> str:
    result = hashlib.sha1()
    with image.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def verify(image: Path, control: Path, pinned: Path) -> None:
    if update_information(image) != UPDATE_INFORMATION:
        raise ValueError("embedded update information does not match the release channel")
    with control.open("rb") as stream:
        prefix = stream.read(65536)
    header, separator, _ = prefix.partition(b"\n\n")
    if not separator:
        raise ValueError("missing or oversized zsync header")
    fields = {}
    for line in header.decode("ascii").splitlines():
        key, colon, value = line.partition(": ")
        if not colon or key in fields:
            raise ValueError("invalid or duplicate zsync header field")
        fields[key] = value
    if fields.get("Filename") != ALIAS or fields.get("URL") != ALIAS:
        raise ValueError("zsync Filename and URL must address the relative AppImage alias")
    length = int(fields["Length"])
    blocksize = int(fields["Blocksize"])
    hash_lengths = [int(v) for v in fields["Hash-Lengths"].split(",")]
    if blocksize <= 0 or len(hash_lengths) != 3 or any(v <= 0 for v in hash_lengths):
        raise ValueError("invalid zsync block parameters")
    body_size = control.stat().st_size - len(header) - 2
    if body_size != ((length + blocksize - 1) // blocksize) * sum(hash_lengths[1:]):
        raise ValueError("zsync block checksum table has the wrong length")
    if length != image.stat().st_size or fields.get("SHA-1") != digest(image):
        raise ValueError("zsync SHA-1/Length do not match the AppImage")
    with image.open("rb") as alias_stream, pinned.open("rb") as pinned_stream:
        while True:
            left = alias_stream.read(1024 * 1024)
            if left != pinned_stream.read(1024 * 1024):
                raise ValueError("AppImage alias and pinned file are not byte-identical")
            if not left:
                break


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("zsync", type=Path)
    parser.add_argument("pinned", type=Path)
    args = parser.parse_args()
    try:
        verify(args.image, args.zsync, args.pinned)
    except (OSError, ValueError, KeyError, UnicodeError, struct.error) as error:
        parser.exit(1, f"AppImage update verification failed: {error}\n")
    print(f"AppImage update verification passed: {UPDATE_INFORMATION}")


if __name__ == "__main__":
    main()
