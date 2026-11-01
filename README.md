# jxlall

Converts a photo library to JPEG XL, keeping pixels, color profiles,
EXIF/XMP and HDR gain maps wherever the format allows. Single binary,
converts files in parallel.

## Install

Download a binary for Linux, macOS or Windows (x86-64 or arm64) from
[Releases](https://github.com/Miosp/jxlall/releases). On Linux, the `musl`
builds are fully static; the `gnu` builds need glibc 2.35 and libstdc++
from GCC 11 or newer. Each release lists SHA-256 checksums, and each
archive has a GitHub build attestation (`gh attestation verify <archive> --repo Miosp/jxlall`).

To build from source you need Rust 1.93+, CMake and a C++ compiler (libjxl is
compiled in): `cargo install --locked --git https://github.com/Miosp/jxlall`.

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

## Artifact removal

`--dejpeg` removes JPEG blocking and ringing with
[DeJPEGNet v2](https://github.com/Miosp/dejpeg) before encoding. It applies to
JPEG inputs; `--dejpeg-all` extends it to every input, except HDR, floating-point
and premultiplied-alpha images, which are encoded unchanged with a note.

Restored JPEGs are no longer recompressed losslessly, so the original JPEG cannot
be recovered from the output. No other setting changes: with the default `-d 0`,
restored images are stored losslessly and are usually much larger than the source,
so you will normally want `-d 1` or similar. Metadata, color profiles, alpha and
gain maps are kept.

On first use jxlall downloads ONNX Runtime, a GPU backend and the model to the user
cache. All of them are pinned by SHA-256 and checked on every run. **This means
jxlall downloads and runs native code from Microsoft, and from NVIDIA where
applicable.** `--dejpeg-backend cpu` limits that to the ONNX Runtime core.

| Platform | `auto` order |
|---|---|
| Linux x86-64, arm64 (`gnu` builds) | TensorRT RTX, WebGPU, CPU |
| Windows x86-64, arm64 | TensorRT RTX, DirectML, WebGPU, CPU |
| macOS arm64 | WebGPU, CPU |
| Linux `musl`, macOS x86-64 | not available |

TensorRT RTX is only tried when an NVIDIA GPU is detected. It needs an RTX GPU
(GeForce RTX 30 series or newer) and a current driver, and downloads about 100-150
MB. On Windows, reaching the DirectML step downloads about 200 MB once; pick
`--dejpeg-backend webgpu` to skip it. Windows also needs the Visual C++ runtime.

Expect minutes per photo on CPU and seconds on a discrete GPU.

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
      --dejpeg             remove JPEG artifacts from JPEG inputs
      --dejpeg-all         with --dejpeg: process every input
      --dejpeg-backend <B> auto, tensorrt-rtx, directml, webgpu or cpu (default auto)
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

The source code is MIT. A built binary is not: it statically links `heic`,
`zenwebp` and `rav1d-safe` (AGPL-3.0, or commercial from Imazen) and
`jpegxl-sys` (GPL-3.0, bundling BSD-licensed libjxl), so distributing a
binary means following AGPL-3.0 terms.

`--dejpeg` downloads ONNX Runtime (MIT) and the DeJPEGNet v2 model (Apache-2.0)
from their publishers at run time, so they are not part of the binary.
