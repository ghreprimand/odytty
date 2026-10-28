#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline byte-identity guards for the retained native inventory and notices."""
import hashlib
import json
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
def check():
    inventory = json.loads((ROOT / 'licenses/conpty/runtime-manifest.json').read_text())
    pin = json.loads((ROOT / 'scripts/conpty-runtime/pin.json').read_text())
    assert pin['version'] == inventory['version'] == '1.25.260930003'
    assert len(pin['members']) == 19 and len(inventory['members']) == 3
    assert len({m['name'] for m in pin['members']}) == 19
    assert pin['url'] == 'https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/1.25.260930003/microsoft.windows.console.conpty.1.25.260930003.nupkg'
    for notice in inventory['notices']:
        data = (ROOT / 'licenses/conpty' / notice['path']).read_bytes()
        assert len(data) == notice['size']
        assert hashlib.sha256(data).hexdigest() == notice['sha256']
    producer = (ROOT / 'scripts/conpty-runtime/fixed-producer.rs').read_bytes()
    assert hashlib.sha256(producer).hexdigest() == '7a6b3b964f3b10daf4a2fa3e573c314be587981d4ffb0ef0ca1d90e62d1b7e29'
    for member in inventory['members']:
        expected = next(x for x in pin['members'] if x['name'] == member['package_member'])
        assert (expected['size'], expected['sha256']) == (member['size'], member['sha256'])
    print('conpty_identity_contracts=pass;notices=2;members=3;producer=unchanged')
if __name__ == '__main__':
    check()
