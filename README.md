# jxlall

Converts a photo library to JPEG XL, keeping pixels, color profiles,
EXIF/XMP and HDR gain maps wherever the format allows. Single binary,
uses every core whether you convert one photo or a whole library.

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
| JPEG | Lossless recompression; the original JPEG can be restored byte for byte (`--iqa` can change its XMP, see [Quality rating](#quality-rating)). CMYK JPEGs are converted to RGB and lossless (SOF3) JPEGs are decoded to pixels instead. |
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
so you will normally want `-d 1` or similar. Metadata, color profiles and alpha are
kept, as are HEIF and AVIF gain maps. JPEG gain maps are not kept: those declared
in XMP (Ultra HDR) get a note, others are dropped silently.

See [AI backends](#ai-backends) for downloads and GPU support.

## AI backends

`--dejpeg` and `--iqa` share one backend, chosen with `--ai-backend`. The default,
`auto`, prefers a GPU and moves on to the next backend, ending with the CPU, if one
fails to load; a backend you name explicitly stops the run instead. Once loaded, a
backend that fails on a photo fails that file.

On first use jxlall downloads ONNX Runtime, a GPU backend and the models to the user
cache (`~/.cache/jxlall`, `%LOCALAPPDATA%\jxlall\cache`,
`~/Library/Caches/jxlall`). All of them are pinned by SHA-256 and checked on every
run. **This means jxlall downloads and runs native code from Microsoft, and from
NVIDIA where applicable.** `--ai-backend cpu` limits that to the ONNX Runtime core.

| Platform | `auto` order |
|---|---|
| Linux x86-64, arm64 (`gnu` builds) | TensorRT RTX, WebGPU, CPU |
| Windows x86-64, arm64 | TensorRT RTX, WebGPU, CPU |
| macOS arm64 | WebGPU, CPU |
| Linux `musl`, macOS x86-64 | not available, these builds have no `--dejpeg` or `--iqa` |

TensorRT RTX is only tried when an NVIDIA GPU is detected. It needs an RTX GPU
(GeForce RTX 30 series or newer) and a current driver, and downloads about 100-150
MB. Windows also needs the Visual C++ runtime.

Scores can differ slightly between backends. Artifact removal takes minutes per photo
on CPU and seconds on a discrete GPU.

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
  -e, --effort <1..=10>    encoder effort (default 4 with -d 0, otherwise 7)
  -d, --distance <F32>     lossy distance for pixel inputs (default 0 = lossless)
      --force-lossy-jpeg   re-encode JPEG pixels instead of recompressing
  -j, --jobs <N>           files converted at once, sharing all CPU cores
                           (default: CPU count)
      --delete-originals   delete sources after successful conversion
      --add-noise          add film grain from the source ISO (lossy only;
                           JPEGs also need --force-lossy-jpeg)
      --noise-strength <F> grain multiplier (default 1.0)
      --noise-iso <ISO>    like --add-noise, but one ISO for every file
                           instead of the metadata one
      --dejpeg             remove JPEG artifacts from JPEG inputs
      --dejpeg-all         with --dejpeg: process every input
      --iqa                rate each photo (see Quality rating)
      --ai-backend <B>     auto, tensorrt-rtx, webgpu or cpu (default auto)
      --keep-going         continue after errors
      --dry-run            list what would be converted
  -v, --verbose            log each file's details and timings
```

In a terminal, a progress bar shows files done, time elapsed and time left. The
estimate weighs files by size and starts once the first file has finished, so a
single photo shows its current step instead. A summary follows on stdout: sizes,
speed, per-format breakdown, the slowest file, notes and failures with their
reasons (the first 20; stderr has them all).

Directory scans skip hidden files and do not follow symlinks. Without
`--keep-going` the first failure stops new conversions and the rest are
reported as skipped. Any failure, missing input or unreadable directory
makes the exit status non-zero.

`--add-noise` is for photos that were denoised before conversion. It stores
grain parameters matching the original ISO (scaled for sensor size when
EXIF has the 35 mm focal length), and the decoder synthesizes the grain at
a cost of a few bytes. `--noise-iso` uses one ISO for every file instead,
for photos whose EXIF was stripped; `--noise-strength` still scales it. It
is a full-frame equivalent: multiply the camera ISO by the crop factor
squared (ISO 800 on a 1.5x crop sensor is about 1800).

## Quality rating

`--iqa` scores each photo with two [Charm](https://github.com/FBehrad/Charm)
models (CVPR 2025, DINOv2-small, ONNX exports from
[Miosp/charm-onnx](https://huggingface.co/Miosp/charm-onnx)) and stores both
scores, 0 to 1, in XMP under `https://github.com/Miosp/jxlall/ns/iqa/1.0/`:

- `Technical`: detail and technical quality (`--iqa-technical spaq|koniq10k`,
  default `spaq`).
- `Aesthetic`: aesthetics and composition (`--iqa-aesthetic
  para|aadb|tad66k|baid`, default `para`).

Each score is written with its model id; scores from different models are not
comparable. Charm sees the whole frame at its own aspect ratio, as displayed
(EXIF orientation applied), and keeps the 64 sharpest regions at full
resolution, so intentional blur such as bokeh is judged in context. Photos
are first downscaled to 1024 px; upstream Charm does that only for `spaq`
and `para`, so scores of the other models differ from upstream on larger
images.

`--iqa-stars T2,T3,T4,T5` also writes `xmp:Rating` (1 to 5 stars, read by
Lightroom, darktable, digiKam and Windows) from the mean of both scores:
the values are the minimum for 2, 3, 4 and 5 stars. A rating the source
already has is kept unless `--overwrite-rating`; `xmp:Rating="0"` (unrated)
does not count, a reject (`-1`) does. A photo that cannot be scored fails like any
other conversion error, and its source is kept.

The models are about 90 MB each. Rating costs roughly 1.5 s per photo on a 16-core CPU, before any GPU speedup. A recompressed JPEG
without XMP stays restorable byte for byte (the rating lives in a separate
box); one with XMP gets the rating written into the packet's padding, so
the restored JPEG carries it, or is left unrated with a note when there is
no room.

With `--dejpeg`, rating scores the restored image. See [AI backends](#ai-backends) for
downloads and GPU support. The Charm code and checkpoints are Apache-2.0.

## License

The source code is MIT. A built binary is not: it statically links `heic`,
`zenwebp` and `rav1d-safe` (AGPL-3.0, or commercial from Imazen) and
`jpegxl-sys` (GPL-3.0, bundling BSD-licensed libjxl), so distributing a
binary means following AGPL-3.0 terms.

`--dejpeg` and `--iqa` download ONNX Runtime (MIT), their models (Apache-2.0) and,
when that backend is used, NVIDIA TensorRT RTX under its own license, from their
publishers at run time. None of them are part of the binary.
