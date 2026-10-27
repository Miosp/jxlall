# jxlall

Convert photos to JPEG XL. Single binary, Linux + Windows, parallel via compio
(thread-per-core async I/O + bounded blocking decode/encode pool).

## Inputs

| Format | Path | Notes |
|---|---|---|
| JPEG (8-bit baseline/progressive) | lossless jbrd recompression (`JxlEncoderAddJPEGFrame`) | byte-exact reversible; EXIF/XMP/JUMBF preserved |
| JPEG lossless (SOF3, Huffman) | `jpeg-decoder` pixels → lossless JXL | 8-bit any-component, 9–16-bit grayscale |
| JPEG CMYK/YCCK (8-bit) | `zune-jpeg` pixels | jbrd cannot represent CMYK; falls back automatically |
| HEIC/HEIF (HEVC) | `heic` (pure Rust) | 10/12-bit decoded at native depth |
| AVIF | `heic` + `rav1d-safe` | |
| WebP (still) | `zenwebp` (pure Rust) | animated WebP rejected (stills tool) |
| TIFF / BigTIFF | `tiff` crate | multipage → `name_p1.jxl`, `name_p2.jxl`, …; CMYK and YCbCr (incl. JPEG-compressed) converted to RGB; planar and gray+alpha supported; associated alpha kept premultiplied; subsampled YCbCr only when JPEG-compressed |
| PNG | `png` crate | 8/16-bit; grayscale expanded to RGB (encoded as gray when it carries a gray ICC) |

Not supported, with reason:

- **12-bit lossy JPEG** — no pure-Rust decoder exists.
- **Hierarchical JPEG** (DHP, differential SOF5–7/13–15) — every available
  decoder reads only the first frame; rejected with an error.
- **Arithmetic-coded JPEG** (SOF9–11; SOF13–15 are hierarchical, above) — no
  pure-Rust decoder exists.
- **Animated WebP/HEIF/PNG (APNG)** — stills tool.
- **RGB gain maps** — `heic` 0.1.6 exposes only the gain map Y plane, so
  multichannel `tmap` metadata is detected and the gain map dropped with a
  note. Monochrome gain maps (the common case) are preserved; `heic` scales
  deeper gain maps to 8 bits.

## Color

- CICP preserved natively when the source carries it (HEIF `nclx`, PNG `cICP`,
  AVIF): mapped to JXL color encoding enums (sRGB/BT.2020/P3/DCI-P3
  primaries; sRGB, BT.709/601/2020, linear, PQ, HLG, DCI, gamma 2.2/2.8
  transfer). An unmapped CICP falls back to ICC, else sRGB, with a note.
- Otherwise the source ICC profile is passed through byte-exact, if its
  color space matches the pixels: a gray profile makes the image encode as
  1-channel gray (if R = G = B everywhere, else the profile is dropped with a
  note); a CMYK (or Lab, ...) profile left over after CMYK → RGB
  conversion is dropped with a note.
- PNG `gAMA`/`cHRM` (without `cICP`/`iCCP`/`sRGB`) become a custom JXL color
  encoding; out-of-range or degenerate values are ignored with a note. Untagged float data (TIFF) is encoded as linear sRGB.
- A malformed ICC profile (bad header size or `acsp` signature) is dropped
  with a note and the image converts with default color, rather than failing.
  Same for a gain map that cannot be decoded or whose metadata is unusable:
  the base image still converts.
- HDR: PQ → 10 000 nits, HLG → 1 000 nits intensity target.
- Gain maps (ISO 21496-1, AVIF `tmap` or Apple `hdrgainmap` XMP) go into the
  JXL `jhgm` box: `tmap` metadata is copied verbatim; Apple XMP is converted
  field by field. Typical iPhone gain maps carry only a version and headroom,
  no ISO 21496-1 fields, so they are not preserved (dropped with a note). A
  gain map that cannot be shown to share the base image's orientation (EXIF
  Orientation used as a proxy for `irot`/`imir`) is dropped too. The
  gain map image is re-encoded losslessly as a naked JXL codestream.

## Metadata

EXIF → `Exif` box, XMP → `xml ` box, JPEG metadata via jbrd. Sources:
JPEG APP1/APP2 segments (EXIF/XMP/ICC), PNG eXIf + iTXt XMP
(`XML:com.adobe.xmp`) + ImageMagick `Raw profile type exif` tEXt, HEIF/WebP
container metadata, TIFF (per page, a minimal TIFF/Exif payload is rebuilt
from that page's IFD: descriptive tags incl. IPTC and private tags, plus the
Exif/GPS/Interop sub-IFDs; pixel-layout tags, strips/tiles, SubIFDs and the
ICC tag are left out, XMP tag 700 goes to the `xml ` box; BigTIFF is
rewritten as classic TIFF; MakerNote is copied opaque, so maker notes with
absolute offsets may not parse; malformed metadata is dropped with a note). Exif and XMP boxes are stored
uncompressed (no `brob`) for reader compatibility.

Extended XMP (JPEG packets over 64 KiB, split into
`http://ns.adobe.com/xmp/extension/` segments) is merged into the single
`xml ` box on the pixel paths (CMYK, lossless JPEG, `--force-lossy-jpeg`):
the extension's `rdf:Description`s are appended to the main packet and
`xmpNote:HasExtendedXMP` is removed. Incomplete or unparsable extensions
are dropped with a note, keeping the main packet. On the jbrd path the
`xml ` box holds only the main packet (libjxl rebuilds the original APP1
segment from it); the extension segments are preserved in the JPEG
reconstruction data, so the restored JPEG is byte-exact.

Orientation is stored in the JXL header as metadata; pixels are never
rotated. Exception: HEIF/AVIF, where `heic` already applies `irot`/`imir`
to the pixels, so the header gets identity orientation and the Exif
Orientation tag is reset to 1.

Output names are reserved before writing: `a.jpg` → `a.jxl`; a later input
with the same stem gets `a.png.jxl`, then numeric suffixes (`a.png.2.jxl`).
Paths are compared after resolving the directory, so `a.png ./a.jpg` or a
relative input with an absolute `-o` cannot collide. Existing files are
never overwritten: outputs are published with a no-replace rename, and a
taken name moves on to the next suffix, so reruns produce `a.png.jxl`,
`a.png.2.jxl`, … next to earlier results. Multi-page files (`a_p1.jxl`,
then `a_p1.2.jxl`, …) follow the same scheme; if any page fails, the pages
already written for that file are removed. With `--delete-originals` each
output is fsynced (file and directories up to `-o`) before its source is
deleted; if that sync fails the output is kept and the source is not
deleted. A source that is the same file as an output (same inode on Unix,
same canonical path on Windows) is never deleted. Directory walks
mirror the input structure under `-o`; in-place output lands next to each
source.

## Film grain

`--add-noise` (off by default) is for photos that were denoised before
conversion: it stores JXL photon-noise parameters so the decoder adds
grain matching the original ISO. The grain is generated at decode time and
costs only a few bytes.

- ISO comes from EXIF `PhotographicSensitivity`, falling back to
  `RecommendedExposureIndex` / `ISOSpeed` when it is missing or saturated
  (65535). Files without an ISO get no grain (with a note).
- libjxl models a full-frame sensor, so the ISO is scaled by crop² when EXIF
  has both `FocalLength` and `FocalLengthIn35mmFilm` (phone ISO 800 at crop
  3.5 ≈ full-frame ISO 9800). Without them the ISO is used as-is (with a
  note).
- `--noise-strength F` multiplies that ISO, in (0, 100]; grain amplitude
  grows roughly with √F. Given without `--add-noise` it only warns.
- The grain is mostly luma by format design (chroma about 1/6 of luma);
  chroma noise is not tunable.
- Applies to lossy pixel encodes only (`-d > 0`; JPEGs also need
  `--force-lossy-jpeg`). libjxl calibrates grain for XYB: on lossless
  pixel encodes it comes out colored, and on lossless JPEG recompression it
  appears only above mid-gray (shadows and midtones get none), so both get
  no grain, with a note at startup. Gain maps never get grain.

## CLI

```
jxlall [OPTIONS] [INPUTS]...
  -o, --output <DIR>      output directory (mirrors input structure)
  -e, --effort <1..=10>   encoder effort (default 7)
  -d, --distance <F32>    lossy distance for the pixel path (0 = lossless, default)
      --force-lossy-jpeg  re-encode JPEG pixels instead of jbrd
  -j, --jobs <N>          parallel conversions (default: CPU count, max 128)
  -r, --recursive         walk subdirectories (default: scan top level only)
  -p, --pattern <PAT>     extension filter for directory scans, `|`-separated,
                          case-insensitive; `jpg`, `.jpg`, `*.jpg` are equivalent
                          (default ".jpg|.jpeg|.png|.webp|.heic|.heif|.avif|.tif|.tiff")
      --delete-originals  delete source files after successful conversion
      --add-noise         add decode-time film grain from the source ISO (lossy only)
      --noise-strength <F> grain strength, multiplies the ISO (default 1.0)
  -v, --verbose           per-file logging (format, sizes)
      --keep-going        continue on errors
      --dry-run           list what would be converted
```

A single-line progress counter (`done/total`) is shown on stderr unless
`--verbose` is set.

Directory scans ignore `.gitignore`/`.ignore`/global excludes but skip
hidden files and directories (`._IMG.JPG` AppleDouble files, `.thumbnails/`);
symlinks inside scanned directories are not followed (pass them
explicitly). An input given twice under different spellings, or an explicit
symlink next to its target, is converted once. Missing inputs and unreadable directories are reported and
make the exit status non-zero, but the remaining files are still converted.
Without `--keep-going` a conversion failure stops scheduling new files; the
summary reports them as skipped.

## License

AGPL-3.0-only. Embedded components: libjxl (BSD), `heic`/`zenwebp`
(AGPL/Imazen), `jpegxl-sys` (GPL, statically linked libjxl).
