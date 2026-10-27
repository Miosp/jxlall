# jxlall

Converts a photo library to JPEG XL, keeping pixels, color profiles,
EXIF/XMP and HDR gain maps wherever the format allows. Single binary for Linux and Windows,
converts files in parallel.

## Inputs

| Format | How |
|---|---|
| JPEG | Lossless recompression; the original JPEG can be restored byte for byte. CMYK JPEGs are converted to RGB and lossless (SOF3) JPEGs are decoded to pixels instead. |
| HEIC/HEIF, AVIF | Decoded at native bit depth. |
| WebP, PNG | Stills only. |
| TIFF / BigTIFF | Each page becomes `name_p1.jxl`, `name_p2.jxl`, ... |

Pixel inputs are encoded losslessly unless you pass `-d`.

Not supported: 12-bit lossy, hierarchical and arithmetic-coded JPEG (no
Rust decoder handles them), animated images, and RGB gain maps (dropped;
monochrome ones, the usual kind, are kept).

## What is preserved

- Color: CICP is mapped to the native JXL encoding, otherwise the ICC profile
  is copied as is. Profiles that do not match the pixels, or are malformed,
  are dropped with a note.
- Metadata: EXIF and XMP go into their JXL boxes, including JPEG extended
  XMP and per-page TIFF tags.
- Gain maps (ISO 21496-1 or Apple XMP) go into the `jhgm` box. Most iPhone
  gain maps lack the ISO fields and are dropped.
- Orientation is stored as metadata and pixels are not rotated. HEIF and AVIF
  are the exception, since their decoder already rotates.

Anything dropped is reported, and the image still converts.

## Safety

`a.jpg` becomes `a.jxl`. Existing files are never overwritten: a name clash
gets a suffix instead (`a.png.jxl`, `a.png.2.jxl`). With `--delete-originals` a source is deleted
only after its output has been synced to disk.

## Usage

```
jxlall [OPTIONS] [INPUTS]...
  -o, --output <DIR>       output directory, mirrors the input tree (default: next to sources)
  -r, --recursive          descend into subdirectories
  -p, --pattern <PAT>      extensions to scan, `|`-separated (default: all supported)
  -e, --effort <1..=10>    encoder effort (default 7)
  -d, --distance <F32>     lossy distance for pixel inputs (default 0 = lossless)
      --force-lossy-jpeg   re-encode JPEG pixels instead of recompressing
  -j, --jobs <N>           parallel conversions (default: CPU count)
      --delete-originals   delete sources after successful conversion
      --add-noise          add film grain from the source ISO (lossy only;
                           JPEGs also need --force-lossy-jpeg)
      --noise-strength <F> grain multiplier (default 1.0)
      --keep-going         continue after errors
      --dry-run            list what would be converted
  -v, --verbose            per-file logging
```

Directory scans skip hidden files and do not follow symlinks. Without
`--keep-going` the first failure stops new conversions and the rest are
reported as skipped. Any failure, missing input or unreadable directory
makes the exit status non-zero.

`--add-noise` is for photos that were denoised before conversion. It stores
grain parameters matching the original ISO (scaled for sensor size when
EXIF has the 35 mm focal length), and the decoder synthesizes the grain at
a cost of a few bytes.

## License

AGPL-3.0-only. Bundles libjxl (BSD) via `jpegxl-sys` (GPL); `heic` and `zenwebp`
are AGPL (commercial licenses available from Imazen).
