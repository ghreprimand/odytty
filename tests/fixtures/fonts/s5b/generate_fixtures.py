#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Regenerate licensed owner-shaping subsets and independent references."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unicodedata

import fontTools
from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent
OPTIONS = ["--layout-features=*", "--no-hinting", "--no-glyph-names",
           "--no-recalc-timestamp", "--name-IDs=*", "--name-languages=*"]


def codepoints(case):
    return [int(value.removeprefix("U+"), 16) for value in case["codepoints"]]


def repertoire(face):
    points = {0x20}
    for case in face["cases"]:
        text = "".join(chr(cp) for cp in codepoints(case))
        points.update(map(ord, text))
        points.update(map(ord, unicodedata.normalize("NFKD", text)))
    declared = {int(value[2:], 16) for value in face["required_scalars"]}
    # Khmer intentionally retains the shaper-inserted dotted circle.
    if face["script"] == "khmer" and 0x25CC in declared:
        points.add(0x25CC)
    if points != declared:
        raise ValueError("normalization repertoire differs from the frozen corpus")
    return points


def shape(path, text, tag):
    command = ["hb-shape", "--output-format=json", "--no-glyph-names",
               "--utf8-clusters", "--cluster-level=0", "--direction=ltr",
               "--language=und", "--script=" + tag, "--font-size=upem",
               "--font-funcs=ot", "--shapers=ot", str(path), text]
    result = subprocess.run(command, capture_output=True, text=True,
                            check=True, timeout=15)
    return json.loads(result.stdout)


def rename_face(font, family):
    # OFL subsets are modified fonts. Preserve copyright and license records,
    # but give every platform/language family record a distinct fixture name.
    names = {1: family, 2: "Regular", 3: family + ";subset-v1",
             4: family + " Regular", 6: family.replace(" ", ""),
             16: family, 17: "Regular", 25: family.replace(" ", "")}
    for record in font["name"].names:
        if record.nameID in names:
            record.string = names[record.nameID].encode(record.getEncoding())
    for name_id, value in names.items():
        if name_id != 25:
            font["name"].setName(value, name_id, 3, 1, 0x409)


def make_face(face, sources, output):
    source = sources / face["source_filename"]
    if not source.is_file() or source.stat().st_size > 8 * 1024 * 1024:
        raise ValueError("missing or oversized source face: " + face["source_filename"])
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    if digest != face["source_sha256"]:
        raise ValueError("source hash mismatch: " + face["source_filename"])
    points = repertoire(face)
    spec = ",".join(f"U+{cp:04X}" for cp in sorted(points))
    # Construct the same subset in memory to retain its original glyph-name
    # correspondence before post-table glyph names are dropped on disk.
    options = subset.Options()
    options.parse_opts(OPTIONS)
    original = TTFont(source, recalcTimestamp=False)
    original_names = original.getGlyphOrder()
    original_cmap = original.getBestCmap()
    missing = points - set(original_cmap) - {0x200C, 0x200D}
    if missing:
        raise ValueError("unmapped fixture scalars: " + str(sorted(missing)))
    in_memory = subset.load_font(source, options)
    selection = subset.Subsetter(options)
    selection.populate(unicodes=points)
    selection.subset(in_memory)
    new_ids = {name: index for index, name in enumerate(in_memory.getGlyphOrder())}
    with tempfile.TemporaryDirectory(prefix="odytty-shape-subset-") as temp:
        raw = Path(temp) / face["font"]
        command = ["pyftsubset", str(source), "--unicodes=" + spec,
                   *OPTIONS, "--output-file=" + str(raw)]
        subprocess.run(command, check=True, capture_output=True, timeout=90)
        font = TTFont(raw, recalcTimestamp=False)
        rename_face(font, face["fixture_family"])
        font.save(output)
    final = TTFont(output, recalcTimestamp=False)
    if final["post"].formatType != 3.0:
        raise ValueError("subset retained glyph names")
    if any(table in final for table in ["fpgm", "prep", "cvt "]):
        raise ValueError("subset retained hinting")
    if not points - {0x200C, 0x200D} <= set(final.getBestCmap()):
        raise ValueError("subset lost required Unicode mappings")
    references = []
    for case in face["cases"]:
        text = "".join(chr(cp) for cp in codepoints(case))
        before = shape(source, text, face["script_tag"])
        # A no-circle fixture intentionally lacks the upstream circle mapping.
        # Match that declared coverage in an isolated source copy for this case;
        # all ordinary rows still compare with the untouched source face.
        if case["note"] == "stray-mark-no-circle":
            if 0x25CC in final.getBestCmap():
                raise ValueError("no-circle fixture unexpectedly maps U+25CC")
            with tempfile.TemporaryDirectory(prefix="odytty-shape-coverage-") as temp:
                limited = TTFont(source, recalcTimestamp=False)
                for table in limited["cmap"].tables:
                    table.cmap.pop(0x25CC, None)
                limited_path = Path(temp) / face["source_filename"]
                limited.save(limited_path)
                before = shape(limited_path, text, face["script_tag"])
        after = shape(output, text, face["script_tag"])
        mapped = [dict(g, g=new_ids[original_names[g["g"]]]) for g in before]
        if mapped != after:
            raise ValueError("subset changed shaping: " + case["id"])
        if any(g["g"] == 0 for g in after):
            raise ValueError("missing reference glyph: " + case["id"])
        arrays = [",".join(str(g[key]) for g in after)
                  for key in ["g", "cl", "dx", "dy", "ax"]]
        references.append("\t".join([face["font"], " ".join(case["codepoints"]),
                                      *arrays, case["note"]]))
    return references, {
        "font": face["font"], "subset_sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        "subset_bytes": output.stat().st_size, "glyph_count": final["maxp"].numGlyphs,
        "units_per_em": final["head"].unitsPerEm, "reference_rows": len(references),
        "unicode_repertoire": [f"U+{cp:04X}" for cp in sorted(points)],
        "subset_command": " ".join(["pyftsubset", "source/" + face["source_filename"],
                                      "--unicodes=" + spec, *OPTIONS,
                                      "--output-file=" + face["group"] + "/" + face["font"]]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--group", action="append", default=[])
    args = parser.parse_args()
    faces = json.loads((ROOT / "corpus.json").read_text())["faces"]
    version = subprocess.check_output(["hb-shape", "--version"], text=True).splitlines()[0]
    if version != "hb-shape (HarfBuzz) 14.5.1" or fontTools.version != "4.63.0":
        raise ValueError("fixture regeneration requires the recorded tool versions")
    for group in dict.fromkeys(face["group"] for face in faces):
        if args.group and group not in args.group:
            continue
        folder = ROOT / group
        folder.mkdir(exist_ok=True)
        lines = ["# " + version, "# fontTools " + fontTools.version,
                 "# font units; UTF-8 byte clusters; LTR; language und; cluster level 0; OT",
                 "font\tcodepoints\tglyph_ids\tclusters\tx_offset\ty_offset\tx_advance\tnote"]
        records = []
        for face in (face for face in faces if face["group"] == group):
            refs, record = make_face(face, args.sources, folder / face["font"])
            lines.extend(refs)
            records.append(record)
            print(face["script"], record["subset_bytes"], "bytes,", len(refs), "references", flush=True)
        (folder / "reference.tsv").write_text("\n".join(lines) + "\n")
        (folder / "subsets.json").write_text(json.dumps({"fonttools": fontTools.version,
            "harfbuzz": version, "faces": records}, indent=2) + "\n")


if __name__ == "__main__":
    main()
