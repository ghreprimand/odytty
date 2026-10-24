# SPDX-License-Identifier: GPL-3.0-only
"""Pack two committed OFL subsets into a TrueType collection.

The output models the face order of macOS's Tamil Sangam MN.ttc: face 0
Tamil Sangam MN, face 1 Grantha Sangam MN, face 2 Tamil Sangam MN Bold, face 3
Grantha Sangam MN Bold. The Tamil subset stands in for faces 0 and 2 and the
Grantha subset for faces 1 and 3. Each face keeps its own copy of its tables;
only its table-record offsets differ from the source subset. Only the
standard library is used, and the output is byte-for-byte reproducible.

Run from this directory: python3 generate_collection_fixture.py
"""
import hashlib
import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCES = [
    HERE / "southern-indic" / "Tamil-subset.ttf",
    HERE / "g6" / "Grantha-subset.ttf",
    HERE / "southern-indic" / "Tamil-subset.ttf",
    HERE / "g6" / "Grantha-subset.ttf",
]
OUTPUT = HERE / "collections" / "TamilSangamMN-layout.ttc"


def pad4(data: bytes) -> bytes:
    return data + b"\0" * (-len(data) % 4)


def rebase(face: bytes, delta: int) -> bytes:
    """Return `face` with every table-record offset moved by `delta`."""
    out = bytearray(face)
    (num_tables,) = struct.unpack_from(">H", face, 4)
    for index in range(num_tables):
        at = 12 + index * 16 + 8
        (offset,) = struct.unpack_from(">I", face, at)
        struct.pack_into(">I", out, at, offset + delta)
    return bytes(out)


def main() -> None:
    faces = [pad4(path.read_bytes()) for path in SOURCES]
    header_len = 12 + 4 * len(faces)
    offsets = []
    position = header_len
    for face in faces:
        offsets.append(position)
        position += len(face)
    out = bytearray(b"ttcf")
    out += struct.pack(">HHI", 1, 0, len(faces))
    for offset in offsets:
        out += struct.pack(">I", offset)
    for face, offset in zip(faces, offsets):
        out += rebase(face, offset)
    OUTPUT.write_bytes(bytes(out))
    print(OUTPUT.name, hashlib.sha256(out).hexdigest())


if __name__ == "__main__":
    main()
