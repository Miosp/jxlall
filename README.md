# photo-convert

Convert photos to JPEG XL. Single binary, Linux + Windows, parallel via compio
(thread-per-core async I/O + bounded blocking decode/encode pool).

## Inputs

| Format | Path | Notes |
|---|---|---|
| JPEG (8-bit baseline/progressive) | lossless jbrd recompression (`JxlEncoderAddJPEGFrame`) | byte-exact reversible; EXIF/XMP/JUMBF preserved |
| JPEG lossless (SOF3/5/7, Huffman) | `jpeg-decoder` pixels → lossless JXL | 8-bit any-component, 9–16-bit grayscale |
| JPEG CMYK/YCCK (8-bit) | `zune-jpeg` pixels | jbrd cannot represent CMYK; falls back automatically |
| HEIC/HEIF (HEVC) | `heic` (pure Rust) | 10/12-bit decoded at native depth |
| AVIF | `heic` + `rav1d-safe` | |
| WebP (still) | `zenwebp` (pure Rust) | animated WebP rejected (stills tool) |
| TIFF | `tiff` crate | multipage → `name_p1.jxl`, `name_p2.jxl`, …; CMYK converted to RGB |
| PNG | `png` crate | 8/16-bit; grayscale expanded to RGB |

Not supported, with reason:

- **12-bit lossy JPEG** — no pure-Rust decoder exists.
- **Arithmetic-coded JPEG** (SOF9/11/13/15) — no pure-Rust decoder exists.
- **Animated WebP/HEIF/PNG (APNG)** — stills tool.
- **RGB gain maps** — `heic` 0.1.6 exposes only the gain map Y plane;
  monochrome gain maps (the common case) are preserved exactly.

## Color

- CICP preserved natively when the source carries it (HEIF `nclx`, PNG `cICP`,
  AVIF): mapped to JXL color encoding enums ( primaries/TF/white point ).
- Otherwise the source ICC profile is passed through byte-exact.
- HDR: PQ → 10 000 nits, HLG → 1 000 nits intensity target.
- Gain maps (ISO 21496-1, AVIF `tmap` or Apple `hdrgainmap` XMP) are
  re-serialized into the JXL `jhgm` box; the gain map image is re-encoded
  losslessly as a naked JXL codestream.

## Metadata

EXIF → `Exif` box, XMP → `xml ` box, JPEG metadata via jbrd. Sources:
JPEG APP1/APP2 segments (EXIF/XMP/ICC), PNG eXIf + iTXt XMP
(`XML:com.adobe.xmp`) + ImageMagick `Raw profile type exif` tEXt, HEIF/WebP
container metadata, TIFF (whole file stored as the Exif payload — a TIFF is
a valid TIFF/EXIF structure, so its IFDs, XMP tag 700 and private tags all
survive). Orientation is stored in the JXL header as metadata, pixels are
never rotated.

Output names are reserved before writing: `a.jpg` → `a.jxl`; a later input
with the same stem gets `a.png.jxl`, then numeric suffixes. Input files are
never overwritten by outputs, and multi-page files (`a_p1.jxl`, …) follow
the same reservation. Directory walks mirror the input structure under
`-o`; in-place output lands next to each source.

## CLI

```
photo-convert [OPTIONS] [INPUTS]...
  -o, --output <DIR>      output directory (mirrors input structure)
  -e, --effort <1..=10>   encoder effort (default 7)
  -d, --distance <F32>    lossy distance for the pixel path (0 = lossless, default)
      --force-lossy-jpeg  re-encode JPEG pixels instead of jbrd
  -j, --jobs <N>          parallel conversions (default: CPU count)
  -r, --recursive         walk subdirectories (default: scan top level only)
  -p, --pattern <PAT>     extension filter for directory scans, `|`-separated
                          (default ".jpg|.jpeg|.png|.webp|.heic|.heif|.avif|.tif|.tiff")
      --delete-originals  delete source files after successful conversion
  -v, --verbose           per-file logging (format, sizes)
      --keep-going        continue on errors
      --dry-run           list what would be converted
```

A single-line progress counter (`done/total`) is shown on stderr unless
`--verbose` is set.

## License

AGPL-3.0-only. Embedded components: libjxl (BSD), `heic`/`zenwebp`
(AGPL/Imazen), `jpegxl-sys` (GPL, statically linked libjxl).
