#!/usr/bin/env python3
"""Offline release-control fixtures; no AppImage fixture is executed."""
import hashlib
from pathlib import Path
import runpy
import struct
import tempfile
import unittest

MODULE = runpy.run_path(str(Path(__file__).with_name("appimage-update.py")))
VERIFY = MODULE["verify"]
INFO = MODULE["UPDATE_INFORMATION"]
ALIAS = MODULE["ALIAS"]


def image_bytes(info=INFO):
    names = b"\0.shstrtab\0.upd_info\0"
    payload = info.encode() + b"\0" * 4
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<Q", header, 40, 64)
    struct.pack_into("<HHH", header, 58, 64, 3, 1)
    sections = bytearray(192)
    struct.pack_into("<IIQQQQIIQQ", sections, 64, 1, 3, 0, 0, 256, len(names), 0, 0, 1, 0)
    struct.pack_into("<IIQQQQIIQQ", sections, 128, 11, 1, 0, 0, 256 + len(names), len(payload), 0, 0, 1, 0)
    return bytes(header + sections + names + payload)


def control_bytes(image, **overrides):
    fields = {"zsync": "0.6.2", "Filename": ALIAS, "Blocksize": "2048",
              "Length": str(len(image)), "Hash-Lengths": "2,2,4", "URL": ALIAS,
              "SHA-1": hashlib.sha1(image).hexdigest()}
    fields.update(overrides)
    return "\n".join(f"{k}: {v}" for k, v in fields.items()).encode() + b"\n\n" + b"\0" * 6


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.image, self.control, self.pinned = (root / name for name in (ALIAS, ALIAS + ".zsync", "odytty-1.2.3-x86_64.AppImage"))
        self.payload = image_bytes()
        self.image.write_bytes(self.payload)
        self.pinned.write_bytes(self.payload)
        self.control.write_bytes(control_bytes(self.payload))

    def verify(self):
        VERIFY(self.image, self.control, self.pinned)

    def test_valid_alias_and_version_pinned_pair(self):
        self.verify()

    def test_wrong_update_channel_fails(self):
        self.image.write_bytes(image_bytes("gh-releases-zsync|example|terminal|latest|terminal.zsync"))
        with self.assertRaisesRegex(ValueError, "embedded update"):
            self.verify()

    def test_non_elf_and_truncated_tables_fail(self):
        for payload in (b"not an ELF", self.payload[:80]):
            with self.subTest(size=len(payload)):
                self.image.write_bytes(payload)
                with self.assertRaises(ValueError):
                    self.verify()

    def test_missing_update_section_fails(self):
        self.image.write_bytes(self.payload.replace(b".upd_info", b".bad_info"))
        with self.assertRaisesRegex(ValueError, "one .upd_info"):
            self.verify()

    def test_missing_control_file_fails(self):
        self.control.unlink()
        with self.assertRaises(OSError):
            self.verify()

    def test_alias_url_and_filename_are_required(self):
        for field in ("URL", "Filename"):
            self.control.write_bytes(control_bytes(self.payload, **{field: self.pinned.name}))
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "relative AppImage alias"):
                self.verify()

    def test_wrong_length_and_digest_fail(self):
        for fields in ({"Length": str(len(self.payload) + 1)}, {"SHA-1": "0" * 40}):
            self.control.write_bytes(control_bytes(self.payload, **fields))
            with self.subTest(fields=fields), self.assertRaisesRegex(ValueError, "SHA-1/Length"):
                self.verify()

    def test_truncated_checksum_body_fails(self):
        self.control.write_bytes(control_bytes(self.payload)[:-1])
        with self.assertRaisesRegex(ValueError, "checksum table"):
            self.verify()

    def test_duplicate_header_and_invalid_block_parameters_fail(self):
        for payload in (control_bytes(self.payload).replace(b"\n\n", b"\nURL: duplicate\n\n"),
                        control_bytes(self.payload, Blocksize="0"),
                        control_bytes(self.payload, **{"Hash-Lengths": "2,2"})):
            self.control.write_bytes(payload)
            with self.assertRaises(ValueError):
                self.verify()

    def test_pinned_bytes_must_match_even_at_same_length(self):
        self.pinned.write_bytes(self.payload[:-1] + b"x")
        with self.assertRaisesRegex(ValueError, "byte-identical"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
