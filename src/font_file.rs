// SPDX-License-Identifier: GPL-3.0-only
//! Bounded reads for font files selected directly or found during discovery.
//!
//! Font parsing owns the returned bytes, so every caller needs a complete file.
//! This boundary rejects non-regular targets and stops one byte past a generous
//! ceiling, preventing a malformed filesystem entry from turning discovery into
//! an unbounded allocation. Symlinks that resolve to ordinary font files remain
//! supported because system font installations commonly use them.
//!
//! # Collections are read one face at a time
//!
//! A TrueType *collection* (`.ttc`) holds many faces that share most of their
//! tables, so the file can be enormous while any single face is ordinary:
//! Iosevka ships 162 faces in 377.1 MiB, of which one face is 9.4 MiB. Reading
//! the whole file to rasterize one glyph was both the reason a legitimate host
//! font failed the size ceiling and, on its own terms, 40x more resident bytes
//! than the job needs.
//!
//! [`read_font_face`] therefore reads a collection's *face* rather than its
//! file: the table directory for the requested face, then only the tables that
//! face references, reassembled as a standalone single-face font. The ceiling
//! is unchanged and now applies to what is actually retained.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Upper bound for one font file. Installed text and emoji fonts are normally
/// far smaller; 256 MiB leaves headroom for large color-emoji collections
/// while bounding both discovery probes and explicit font-path loads.
pub(crate) const MAX_FONT_FILE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded(path, MAX_FONT_FILE_BYTES)
}

/// Magic of a TrueType collection header.
const TTC_TAG: [u8; 4] = *b"ttcf";
/// Bytes in one sfnt table-directory record: tag, checksum, offset, length.
const TABLE_RECORD_BYTES: u64 = 16;
/// Bytes in an sfnt header before the table records begin.
const SFNT_HEADER_BYTES: u64 = 12;
/// Upper bound on table-directory entries in one face.
///
/// The real sfnt maximum is a `u16`, and a genuine face has a few dozen. This
/// bounds the directory read from a corrupt or hostile header before any
/// allocation is sized from it.
const MAX_TABLES_PER_FACE: u16 = 512;

/// Read one face of a font file as a standalone single-face font.
///
/// For an ordinary single-face file this is [`read_font_file`] and
/// `face_index` must be 0. For a collection it reads only the requested face's
/// tables, so the cost is the face rather than the file.
///
/// Every offset and length here comes from a file on disk that OdyTTY did not
/// write, so all of them are treated as untrusted: bounds are checked against
/// the real file length before a seek, arithmetic is checked, and the assembled
/// size is compared against the ceiling *before* any buffer is reserved.
pub(crate) fn read_font_face(path: &Path, face_index: u32) -> io::Result<Vec<u8>> {
    let file_len = regular_file_len(path)?;
    let mut file = open_for_read(path)?;

    let mut magic = [0u8; 4];
    // A file too short to hold a magic number cannot be either kind of font;
    // fall through to the whole-file path so it fails where it always did.
    if file_len < 4 || file.read_exact(&mut magic).is_err() || magic != TTC_TAG {
        if face_index != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("font is not a collection, so face {face_index} does not exist"),
            ));
        }
        return read_font_file(path);
    }

    let face_offset = collection_face_offset(&mut file, file_len, face_index)?;
    extract_face(&mut file, file_len, face_offset, MAX_FONT_FILE_BYTES)
}

/// Byte offset of face `face_index` within a collection.
fn collection_face_offset(file: &mut File, file_len: u64, face_index: u32) -> io::Result<u64> {
    // ttcf header: tag(4) version(4) numFonts(4) then one u32 offset per font.
    file.seek(SeekFrom::Start(8))?;
    let num_fonts = read_u32(file)?;
    if face_index >= num_fonts {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("collection has {num_fonts} faces, so face {face_index} does not exist"),
        ));
    }
    // Seek directly to the wanted entry rather than reading the whole table:
    // `num_fonts` is attacker-influenced and a 4-billion-entry read is exactly
    // the unbounded allocation this module exists to prevent.
    let entry = 12u64
        .checked_add((face_index as u64).checked_mul(4).ok_or_else(bad_offset)?)
        .ok_or_else(bad_offset)?;
    if entry.checked_add(4).ok_or_else(bad_offset)? > file_len {
        return Err(bad_offset());
    }
    file.seek(SeekFrom::Start(entry))?;
    let offset = read_u32(file)? as u64;
    if offset
        .checked_add(SFNT_HEADER_BYTES)
        .ok_or_else(bad_offset)?
        > file_len
    {
        return Err(bad_offset());
    }
    Ok(offset)
}

/// Reassemble the face at `face_offset` as a standalone font.
fn extract_face(
    file: &mut File,
    file_len: u64,
    face_offset: u64,
    limit: u64,
) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(face_offset))?;
    let sfnt_version = read_u32(file)?;
    let num_tables = read_u16(file)?;
    if num_tables == 0 || num_tables > MAX_TABLES_PER_FACE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("font face declares {num_tables} tables"),
        ));
    }
    // Skip searchRange / entrySelector / rangeShift: they are derived values,
    // recomputed below rather than trusted from the source.
    file.seek(SeekFrom::Current(6))?;

    let directory_bytes = (num_tables as u64)
        .checked_mul(TABLE_RECORD_BYTES)
        .ok_or_else(bad_offset)?;
    if face_offset
        .checked_add(SFNT_HEADER_BYTES)
        .and_then(|v| v.checked_add(directory_bytes))
        .ok_or_else(bad_offset)?
        > file_len
    {
        return Err(bad_offset());
    }

    // Read the directory, validating every entry against the real file length.
    let mut entries: Vec<([u8; 4], u32, u32, u32)> = Vec::with_capacity(num_tables as usize);
    let mut payload_bytes = 0u64;
    for _ in 0..num_tables {
        let mut tag = [0u8; 4];
        file.read_exact(&mut tag)?;
        let checksum = read_u32(file)?;
        let offset = read_u32(file)?;
        let length = read_u32(file)?;
        if (offset as u64)
            .checked_add(length as u64)
            .ok_or_else(bad_offset)?
            > file_len
        {
            return Err(bad_offset());
        }
        payload_bytes = payload_bytes
            .checked_add(padded_len(length as u64))
            .ok_or_else(bad_offset)?;
        entries.push((tag, checksum, offset, length));
    }

    let total = SFNT_HEADER_BYTES
        .checked_add(directory_bytes)
        .and_then(|v| v.checked_add(payload_bytes))
        .ok_or_else(bad_offset)?;
    // Checked against the ceiling BEFORE reserving, so a corrupt directory
    // cannot drive an allocation the limit is meant to forbid.
    if total > limit {
        return Err(over_limit(total, limit));
    }

    // Tables are emitted in directory order; the directory itself is sorted by
    // tag, which the sfnt specification requires and which some parsers rely on
    // for binary search.
    entries.sort_by_key(|(tag, _, _, _)| *tag);

    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&sfnt_version.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    let (search_range, entry_selector, range_shift) = binary_search_params(num_tables);
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    // Directory first with the new offsets, then the payloads in the same
    // order, so the two cannot disagree.
    let mut cursor = SFNT_HEADER_BYTES + directory_bytes;
    for (tag, checksum, _, length) in &entries {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&(cursor as u32).to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        cursor += padded_len(*length as u64);
    }
    for (_, _, offset, length) in &entries {
        file.seek(SeekFrom::Start(*offset as u64))?;
        let before = out.len();
        file.by_ref()
            .take(*length as u64)
            .read_to_end(&mut out)
            .map_err(|_| bad_offset())?;
        if out.len() - before != *length as usize {
            // The file changed under the read, or the directory lied about a
            // length that the length check could not catch.
            return Err(bad_offset());
        }
        let pad = padded_len(*length as u64) - *length as u64;
        out.resize(out.len() + pad as usize, 0);
    }
    Ok(out)
}

/// Table payloads are padded to a four-byte boundary in an sfnt file.
fn padded_len(length: u64) -> u64 {
    length.div_ceil(4).saturating_mul(4)
}

/// `searchRange` / `entrySelector` / `rangeShift`, recomputed for the emitted
/// directory rather than copied from the source, so they describe the font
/// actually produced.
fn binary_search_params(num_tables: u16) -> (u16, u16, u16) {
    let mut entry_selector = 0u16;
    while (1u32 << (entry_selector + 1)) <= num_tables as u32 {
        entry_selector += 1;
    }
    let search_range = (1u32 << entry_selector) as u16 * 16;
    let range_shift = num_tables * 16 - search_range;
    (search_range, entry_selector, range_shift)
}

fn read_u32(file: &mut File) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

fn read_u16(file: &mut File) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf)?;
    Ok(u16::from_be_bytes(buf))
}

fn bad_offset() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "font table directory points outside the file",
    )
}

/// Length of `path`, rejecting anything that is not a regular file.
fn regular_file_len(path: &Path) -> io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "font path is not a regular file",
        ));
    }
    Ok(metadata.len())
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    // Check before opening so directories, devices, sockets, and FIFOs are
    // rejected without attempting a potentially blocking read. metadata()
    // follows a symlink to its target, preserving existing installed-font and
    // explicit-path behavior when that target is a regular file.
    let path_metadata = std::fs::metadata(path)?;
    if !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "font path is not a regular file",
        ));
    }
    if path_metadata.len() > limit {
        return Err(over_limit(path_metadata.len(), limit));
    }

    let file = open_for_read(path)?;
    // Validate the opened object as well. This catches a path changed after the
    // pre-open check whenever the replacement can be opened without blocking.
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened font path is not a regular file",
        ));
    }
    if opened_metadata.len() > limit {
        return Err(over_limit(opened_metadata.len(), limit));
    }

    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(over_limit(bytes.len() as u64, limit));
    }
    Ok(bytes)
}

fn open_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

fn over_limit(found: u64, limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("font file is {found} bytes, over the {limit}-byte limit"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-font-read-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("create temp directory");
        path
    }

    #[test]
    fn accepts_the_limit_and_rejects_limit_plus_one() {
        let dir = temp_dir("cap");
        let path = dir.join("font.bin");
        fs::write(&path, b"0123456789abcdef").expect("write at-limit file");
        assert_eq!(read_bounded(&path, 16).expect("read at limit").len(), 16);

        fs::write(&path, b"0123456789abcdefg").expect("write over-limit file");
        let error = read_bounded(&path, 16).expect_err("reject limit plus one");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("over the 16-byte limit"));
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    #[test]
    fn rejects_a_non_regular_path() {
        let dir = temp_dir("type");
        let error = read_bounded(&dir, 16).expect_err("reject directory");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// Build a synthetic collection: `faces` faces, each with one table whose
    /// payload is a distinct fill byte, so an extraction can be checked against
    /// the face it claims to be.
    fn synthetic_collection(faces: &[(u8, u32)]) -> Vec<u8> {
        let n = faces.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&n.to_be_bytes());
        // Face offsets follow the header; each face is a 12-byte sfnt header
        // plus one 16-byte record, then its payload.
        let dir_end = 12 + 4 * n as usize;
        let mut offsets = Vec::new();
        let mut bodies = Vec::new();
        let mut cursor = dir_end;
        for (fill, len) in faces {
            offsets.push(cursor as u32);
            let mut face = Vec::new();
            face.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfntVersion
            face.extend_from_slice(&1u16.to_be_bytes()); // numTables
            face.extend_from_slice(&0u16.to_be_bytes()); // searchRange
            face.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
            face.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
            let payload_at = cursor + 12 + 16;
            face.extend_from_slice(b"TEST");
            face.extend_from_slice(&0u32.to_be_bytes()); // checksum
            face.extend_from_slice(&(payload_at as u32).to_be_bytes());
            face.extend_from_slice(&len.to_be_bytes());
            face.extend(std::iter::repeat_n(*fill, *len as usize));
            cursor += face.len();
            bodies.push(face);
        }
        for offset in &offsets {
            out.extend_from_slice(&offset.to_be_bytes());
        }
        for body in bodies {
            out.extend_from_slice(&body);
        }
        out
    }

    fn write_temp(tag: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let path = dir.join("font.ttc");
        fs::write(&path, bytes).expect("write synthetic collection");
        (dir, path)
    }

    /// Each face extracts its own payload, not face 0's.
    #[test]
    fn extracts_the_requested_face_of_a_collection() {
        let bytes = synthetic_collection(&[(0xAA, 32), (0xBB, 64), (0xCC, 16)]);
        let (dir, path) = write_temp("ttc-face", &bytes);
        for (index, fill, len) in [(0u32, 0xAAu8, 32usize), (1, 0xBB, 64), (2, 0xCC, 16)] {
            let face = read_font_face(&path, index).expect("extract face");
            // sfnt header + one record, then the payload.
            assert_eq!(&face[0..4], &0x0001_0000u32.to_be_bytes());
            assert_eq!(u16::from_be_bytes([face[4], face[5]]), 1);
            assert_eq!(&face[12..16], b"TEST");
            let payload = &face[28..28 + len];
            assert!(
                payload.iter().all(|b| *b == fill),
                "face {index} extracted another face's payload"
            );
        }
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// One face is a fraction of its collection: the whole point of the path.
    #[test]
    fn a_face_is_far_smaller_than_its_collection() {
        let bytes = synthetic_collection(&[(0xAA, 4096); 40]);
        let (dir, path) = write_temp("ttc-size", &bytes);
        let face = read_font_face(&path, 7).expect("extract face");
        assert!(
            face.len() * 20 < bytes.len(),
            "face is {} bytes of a {}-byte collection",
            face.len(),
            bytes.len()
        );
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// A face index past the end is refused rather than silently clamped to 0 --
    /// clamping would render the wrong weight and look like success.
    #[test]
    fn rejects_a_face_index_past_the_end() {
        let bytes = synthetic_collection(&[(0xAA, 32), (0xBB, 32)]);
        let (dir, path) = write_temp("ttc-range", &bytes);
        let error = read_font_face(&path, 2).expect_err("reject out-of-range face");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("face 2 does not exist"));
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// A single-face file still loads at index 0, and refuses any other index
    /// rather than returning face 0 under a different name.
    #[test]
    fn plain_font_files_load_only_at_face_zero() {
        let dir = temp_dir("plain");
        let path = dir.join("font.ttf");
        fs::write(&path, b"\x00\x01\x00\x00rest-of-a-plain-font").expect("write plain font");
        assert_eq!(
            read_font_face(&path, 0).expect("plain font at face 0"),
            b"\x00\x01\x00\x00rest-of-a-plain-font"
        );
        let error = read_font_face(&path, 1).expect_err("reject face 1 of a single-face file");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// A table directory pointing outside the file is refused, not read.
    ///
    /// These offsets come from a file OdyTTY did not write, so they are
    /// adversarial input: the check must happen before the seek and before any
    /// buffer is sized from them.
    #[test]
    fn rejects_a_table_pointing_outside_the_file() {
        let mut bytes = synthetic_collection(&[(0xAA, 32)]);
        // The single table record's length field, set past the end of the file.
        let record_at = 12 + 4 + 12;
        let length_at = record_at + 12;
        bytes[length_at..length_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let (dir, path) = write_temp("ttc-oob", &bytes);
        let error = read_font_face(&path, 0).expect_err("reject out-of-bounds table");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// An absurd table count is refused before it can size an allocation.
    #[test]
    fn rejects_an_absurd_table_count() {
        let mut bytes = synthetic_collection(&[(0xAA, 32)]);
        let num_tables_at = 12 + 4 + 4;
        bytes[num_tables_at..num_tables_at + 2].copy_from_slice(&u16::MAX.to_be_bytes());
        let (dir, path) = write_temp("ttc-tables", &bytes);
        let error = read_font_face(&path, 0).expect_err("reject absurd table count");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    /// A truncated collection header fails cleanly rather than panicking.
    #[test]
    fn truncated_collections_fail_without_panicking() {
        let full = synthetic_collection(&[(0xAA, 32), (0xBB, 32)]);
        for cut in [4usize, 8, 10, 14, 20, 30] {
            let dir = temp_dir(&format!("ttc-cut-{cut}"));
            let path = dir.join("font.ttc");
            fs::write(&path, &full[..cut.min(full.len())]).expect("write truncated collection");
            // Either error is acceptable; a panic or a hang is not.
            let _ = read_font_face(&path, 0);
            let _ = read_font_face(&path, 1);
            fs::remove_dir_all(dir).expect("remove temp directory");
        }
    }

    /// The extracted directory is sorted by tag and its offsets are internally
    /// consistent, so a parser that binary-searches the directory finds tables
    /// where the directory says they are.
    #[test]
    fn extracted_directory_is_sorted_and_self_consistent() {
        // Three tables in deliberately unsorted order.
        let mut face = Vec::new();
        face.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        face.extend_from_slice(&3u16.to_be_bytes());
        face.extend_from_slice(&0u16.to_be_bytes());
        face.extend_from_slice(&0u16.to_be_bytes());
        face.extend_from_slice(&0u16.to_be_bytes());
        let payload_base = 12 + 4 + 12 + 3 * 16;
        // Lengths deliberately not multiples of four, to exercise padding.
        for (tag, len, at) in [
            (b"zzzz", 5u32, payload_base),
            (b"aaaa", 7u32, payload_base + 8),
            (b"mmmm", 3u32, payload_base + 16),
        ] {
            face.extend_from_slice(tag);
            face.extend_from_slice(&0u32.to_be_bytes());
            face.extend_from_slice(&(at as u32).to_be_bytes());
            face.extend_from_slice(&len.to_be_bytes());
        }
        face.extend(std::iter::repeat_n(0x11u8, 24));

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ttcf");
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(&face);

        let (dir, path) = write_temp("ttc-sorted", &bytes);
        let out = read_font_face(&path, 0).expect("extract face");
        let count = u16::from_be_bytes([out[4], out[5]]) as usize;
        assert_eq!(count, 3);
        let mut previous: Option<[u8; 4]> = None;
        for i in 0..count {
            let at = 12 + i * 16;
            let tag: [u8; 4] = out[at..at + 4].try_into().expect("tag");
            let offset = u32::from_be_bytes(out[at + 8..at + 12].try_into().expect("offset"));
            let length = u32::from_be_bytes(out[at + 12..at + 16].try_into().expect("length"));
            if let Some(previous) = previous {
                assert!(previous < tag, "directory is not sorted by tag");
            }
            previous = Some(tag);
            assert!(
                offset as usize + length as usize <= out.len(),
                "table {tag:?} runs past the extracted font"
            );
            assert_eq!(offset % 4, 0, "table {tag:?} is not four-byte aligned");
        }
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_symlinks_to_regular_font_files() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink");
        let target = dir.join("target.bin");
        let link = dir.join("font.bin");
        fs::write(&target, b"font").expect("write target");
        symlink(&target, &link).expect("create font symlink");
        assert_eq!(read_bounded(&link, 16).expect("read linked font"), b"font");
        fs::remove_dir_all(dir).expect("remove temp directory");
    }
}
