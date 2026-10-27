//! Minimal per-page Exif payload for TIFF input: a fresh classic TIFF holding
//! one page's descriptive IFD0 tags plus its Exif/GPS/Interop sub-IFDs, with
//! no pixel data and no other pages. Source byte order is preserved; values
//! are copied as raw bytes, so MakerNote internal offsets are not rewritten.

const EXIF_IFD: u16 = 34665;
const GPS_IFD: u16 = 34853;
const INTEROP_IFD: u16 = 40965;
const TYPE_LONG: u16 = 4;
const TYPE_IFD: u16 = 13;
const CLASSIC_MAGIC: u16 = 42;
const BIG_MAGIC: u16 = 43;
const CLASSIC_HEADER_LEN: usize = 8;
const CLASSIC_ENTRY_LEN: usize = 12;
const CLASSIC_INLINE_LEN: usize = 4;
const BIG_ENTRY_LEN: usize = 20;
const BIG_INLINE_LEN: usize = 8;
/// Per-IFD cap; with duplicate tags removed, at most four IFDs are walked.
const MAX_ENTRIES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 16 << 20;

/// IFD0 tags that describe pixel layout or point at pixel/structure data,
/// plus ICC and XMP, which go to the JXL color encoding and `xml ` box.
#[rustfmt::skip]
const EXCLUDED_TAGS: &[u16] = &[
    254, 255,                     // NewSubfileType, SubfileType
    256, 257, 258, 259, 262, 263, // dims, BitsPerSample, Compression, Photometric, Threshholding
    266,                          // FillOrder
    273, 277, 278, 279, 280, 281, // strips, SamplesPerPixel, Min/MaxSampleValue
    284, 288, 289, 290, 291,      // PlanarConfig, FreeOffsets/ByteCounts, GrayResponse
    292, 293,                     // T4/T6 options
    317, 320, 322, 323, 324, 325, // Predictor, ColorMap, tiles
    330,                          // SubIFDs
    332, 333, 334, 336,           // InkSet, InkNames, NumberOfInks, DotRange
    338, 339, 340, 341, 347,      // ExtraSamples, SampleFormat, S{Min,Max}SampleValue, JPEGTables
    512, 513, 514, 515, 517, 518, 519, 520, 521, // old-style JPEG
    529, 530, 531, 532,           // YCbCr coefficients/subsampling/positioning, ReferenceBlackWhite
    32995, 32996, 32997, 32998,   // SGI Matteing, DataType, ImageDepth, TileDepth
    700,                          // XMP
    34675,                        // ICC profile
    37724,                        // Photoshop ImageSourceData (layer pixels)
];

#[derive(Clone, Copy, PartialEq)]
enum Ifd {
    Primary,
    Exif,
    Leaf,
}

impl Ifd {
    fn child(self, tag: u16) -> Option<Ifd> {
        match (self, tag) {
            (Ifd::Primary, EXIF_IFD) => Some(Ifd::Exif),
            (Ifd::Primary, GPS_IFD) | (Ifd::Exif, INTEROP_IFD) => Some(Ifd::Leaf),
            _ => None,
        }
    }

    fn keeps(self, tag: u16) -> bool {
        self != Ifd::Primary || !EXCLUDED_TAGS.contains(&tag)
    }
}

struct Entry<'a> {
    tag: u16,
    ty: u16,
    count: u32,
    value: EntryValue<'a>,
}

enum EntryValue<'a> {
    Bytes(&'a [u8]),
    SubIfd(Ifd, u64),
}

struct Writer<'a> {
    src: &'a [u8],
    big_endian: bool,
    bigtiff: bool,
    out: Vec<u8>,
}

/// Build the Exif payload for the page whose IFD starts at `ifd_offset`.
/// `Ok(None)` when the page has no descriptive tags; `Err` on malformed input.
pub fn minimal_exif(src: &[u8], ifd_offset: u64) -> Result<Option<Vec<u8>>, String> {
    let big_endian = match src.get(..2) {
        Some(b"II") => false,
        Some(b"MM") => true,
        _ => return Err("bad TIFF byte order".into()),
    };
    let bigtiff = match src.get(2..4) {
        Some(m) if m == w16(big_endian, CLASSIC_MAGIC) => false,
        Some(m) if m == w16(big_endian, BIG_MAGIC) => true,
        _ => return Err("bad TIFF magic".into()),
    };
    let mut w = Writer {
        src,
        big_endian,
        bigtiff,
        out: Vec::new(),
    };
    w.out.extend_from_slice(&src[..2]);
    w.put_u16(CLASSIC_MAGIC);
    w.put_u32(CLASSIC_HEADER_LEN as u32);
    if w.copy_ifd(ifd_offset, Ifd::Primary)? == 0 {
        return Ok(None);
    }
    if w.out.len() > MAX_OUTPUT_BYTES {
        return Err("TIFF metadata too large".into());
    }
    Ok(Some(w.out))
}

fn w16(big_endian: bool, v: u16) -> [u8; 2] {
    if big_endian {
        v.to_be_bytes()
    } else {
        v.to_le_bytes()
    }
}

fn w32(big_endian: bool, v: u32) -> [u8; 4] {
    if big_endian {
        v.to_be_bytes()
    } else {
        v.to_le_bytes()
    }
}

impl<'a> Writer<'a> {
    fn bytes(&self, off: u64, len: u64) -> Result<&'a [u8], String> {
        let start = usize::try_from(off).map_err(|_| "offset out of range")?;
        let len = usize::try_from(len).map_err(|_| "length out of range")?;
        start
            .checked_add(len)
            .and_then(|end| self.src.get(start..end))
            .ok_or_else(|| "TIFF value out of bounds".into())
    }

    fn read_uint(&self, off: u64, len: usize) -> Result<u64, String> {
        let b = self.bytes(off, len as u64)?;
        Ok(b.iter().enumerate().fold(0u64, |acc, (i, &x)| {
            let shift = if self.big_endian { len - 1 - i } else { i };
            acc | (u64::from(x) << (8 * shift))
        }))
    }

    fn read_u16(&self, off: u64) -> Result<u16, String> {
        Ok(self.read_uint(off, 2)? as u16)
    }

    fn put_u16(&mut self, v: u16) {
        self.out.extend_from_slice(&w16(self.big_endian, v));
    }

    fn put_u32(&mut self, v: u32) {
        self.out.extend_from_slice(&w32(self.big_endian, v));
    }

    fn patch_u16(&mut self, at: usize, v: u16) {
        self.out[at..at + 2].copy_from_slice(&w16(self.big_endian, v));
    }

    fn patch_u32(&mut self, at: usize, v: u32) {
        self.out[at..at + 4].copy_from_slice(&w32(self.big_endian, v));
    }

    fn out_offset(&self) -> Result<u32, String> {
        if self.out.len() > MAX_OUTPUT_BYTES {
            return Err("TIFF metadata too large".into());
        }
        Ok(self.out.len() as u32)
    }

    /// Append the IFD at `off` (and its sub-IFDs) to the output, returning
    /// the number of entries written.
    fn copy_ifd(&mut self, off: u64, kind: Ifd) -> Result<usize, String> {
        let entries = self.read_entries(off, kind)?;
        self.put_u16(entries.len() as u16);
        let table = self.out.len();
        self.out
            .resize(table + entries.len() * CLASSIC_ENTRY_LEN + 4, 0);
        for (i, e) in entries.iter().enumerate() {
            let at = table + i * CLASSIC_ENTRY_LEN;
            let (ty, count, value) = match e.value {
                EntryValue::Bytes(b) if b.len() <= CLASSIC_INLINE_LEN => {
                    self.out[at + 8..at + 8 + b.len()].copy_from_slice(b);
                    (e.ty, e.count, None)
                }
                EntryValue::Bytes(b) => {
                    let pos = self.out_offset()?;
                    self.out.extend_from_slice(b);
                    if self.out.len() % 2 == 1 {
                        self.out.push(0);
                    }
                    (e.ty, e.count, Some(pos))
                }
                EntryValue::SubIfd(child, child_off) => {
                    let pos = self.out_offset()?;
                    self.copy_ifd(child_off, child)?;
                    (TYPE_LONG, 1, Some(pos))
                }
            };
            self.patch_u16(at, e.tag);
            self.patch_u16(at + 2, ty);
            self.patch_u32(at + 4, count);
            if let Some(pos) = value {
                self.patch_u32(at + 8, pos);
            }
        }
        Ok(entries.len())
    }

    fn read_entries(&self, off: u64, kind: Ifd) -> Result<Vec<Entry<'a>>, String> {
        // (IFD entry-count width, per-entry count width, inline value width, entry width)
        let (n_len, count_len, inline_len, entry_len) = if self.bigtiff {
            (8, 8, BIG_INLINE_LEN, BIG_ENTRY_LEN)
        } else {
            (2, 4, CLASSIC_INLINE_LEN, CLASSIC_ENTRY_LEN)
        };
        let n = self.read_uint(off, n_len)?;
        if n > MAX_ENTRIES as u64 {
            return Err("too many TIFF IFD entries".into());
        }
        let table = off + n_len as u64;
        self.bytes(table, n * entry_len as u64)?;
        let mut entries = Vec::new();
        for i in 0..n {
            let at = table + i * entry_len as u64;
            let tag = self.read_u16(at)?;
            let ty = self.read_u16(at + 2)?;
            let count = self.read_uint(at + 4, count_len)?;
            let value_at = at + 4 + count_len as u64;
            if !kind.keeps(tag) {
                continue;
            }
            if let Some(child) = kind.child(tag) {
                let pointer_len = match ty {
                    TYPE_LONG | TYPE_IFD => 4,
                    16 | 18 if self.bigtiff => 8,
                    _ => continue,
                };
                if count == 1 {
                    let child_off = self.read_uint(value_at, pointer_len)?;
                    entries.push(Entry {
                        tag,
                        ty,
                        count: 1,
                        value: EntryValue::SubIfd(child, child_off),
                    });
                }
                continue;
            }
            // Types beyond classic TIFF's (BigTIFF LONG8/IFD8) cannot be
            // represented; unknown IFD-typed values would keep stale offsets.
            let (Some(size), Ok(count)) = (type_size(ty), u32::try_from(count)) else {
                continue;
            };
            if ty == TYPE_IFD {
                continue;
            }
            let len = u64::from(count) * size as u64;
            let bytes = if len <= inline_len as u64 {
                self.bytes(value_at, len)?
            } else {
                self.bytes(self.read_uint(value_at, inline_len)?, len)?
            };
            entries.push(Entry {
                tag,
                ty,
                count,
                value: EntryValue::Bytes(bytes),
            });
        }
        // Tags must be unique; duplicates would also multiply sub-IFD walks.
        entries.sort_by_key(|e| e.tag);
        entries.dedup_by_key(|e| e.tag);
        Ok(entries)
    }
}

/// Byte size of one value of a classic TIFF field type.
fn type_size(ty: u16) -> Option<usize> {
    match ty {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One classic IFD; every value is a 4-byte LONG or an offset.
    fn ifd(be: bool, entries: &[(u16, u16, u32, u32)]) -> Vec<u8> {
        let mut out = w16(be, entries.len() as u16).to_vec();
        for &(tag, ty, count, value) in entries {
            out.extend(w16(be, tag));
            out.extend(w16(be, ty));
            out.extend(w32(be, count));
            out.extend(w32(be, value));
        }
        out.extend(w32(be, 0));
        out
    }

    fn ifd_len(n: usize) -> u32 {
        (2 + n * CLASSIC_ENTRY_LEN + 4) as u32
    }

    #[test]
    fn big_endian_sub_ifds_are_rewritten() {
        let make = 8 + ifd_len(2);
        let exif_ifd = make + 6;
        let date = exif_ifd + ifd_len(1);
        let mut src = b"MM\0\x2a\0\0\0\x08".to_vec();
        src.extend(ifd(
            true,
            &[(271, 2, 6, make), (EXIF_IFD, TYPE_LONG, 1, exif_ifd)],
        ));
        src.extend(b"Canon\0");
        src.extend(ifd(true, &[(36867, 2, 20, date)]));
        src.extend(b"2024:01:02 03:04:05\0");
        let out = minimal_exif(&src, 8).unwrap().unwrap();
        assert_eq!(&out[..4], b"MM\0\x2a");
        let exif = exif::Reader::new().read_raw(out).unwrap();
        let get = |t| {
            exif.get_field(t, exif::In::PRIMARY)
                .unwrap()
                .display_value()
                .to_string()
        };
        assert_eq!(get(exif::Tag::Make), "\"Canon\"");
        assert_eq!(get(exif::Tag::DateTimeOriginal), "2024-01-02 03:04:05");
    }

    #[test]
    fn duplicate_pointer_tags_are_walked_once() {
        let exif_ifd = 8 + ifd_len(MAX_ENTRIES);
        let leaf = exif_ifd + ifd_len(MAX_ENTRIES);
        let mut src = b"II\x2a\0\x08\0\0\0".to_vec();
        src.extend(ifd(
            false,
            &vec![(EXIF_IFD, TYPE_LONG, 1, exif_ifd); MAX_ENTRIES],
        ));
        src.extend(ifd(
            false,
            &vec![(INTEROP_IFD, TYPE_LONG, 1, leaf); MAX_ENTRIES],
        ));
        // Unknown field type: parsed, then skipped.
        src.extend(ifd(false, &vec![(1, 99, 1, 0); MAX_ENTRIES]));
        let out = minimal_exif(&src, 8).unwrap().unwrap();
        let header_and_ifd0 = CLASSIC_HEADER_LEN + ifd_len(1) as usize;
        assert_eq!(
            out.len(),
            header_and_ifd0 + 2 * ifd_len(1) as usize - CLASSIC_ENTRY_LEN
        );
    }
}
