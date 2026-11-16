# jxlall

Converts a photo library to JPEG XL, or with `-f avif` to AVIF, keeping
pixels, color profiles, EXIF/XMP and HDR gain maps wherever the format
allows. Single binary, uses every core whether you convert one photo or a
whole library.

## Install

Download a binary for Linux, macOS or Windows (x86-64 or arm64) from
[Releases](https://github.com/Miosp/jxlall/releases). On Linux, the `musl`
builds are fully static; the `gnu` builds need glibc 2.35 and libstdc++
from GCC 11 or newer. Each release lists SHA-256 checksums, and each
archive has a GitHub build attestation (`gh attestation verify <archive> --repo Miosp/jxlall`).

To build from source you need Rust 1.93+, CMake, a C++ compiler (libjxl and
SVT-AV1 are compiled in) and, on x86-64, [NASM](https://nasm.us) 2.14+:
`cargo install --locked --git https://github.com/Miosp/jxlall`. A clone needs
`git submodule update --init` for the SVT-AV1 sources. Without NASM, SVT-AV1 is
built without SIMD and AVIF encoding is several times slower.

## Inputs

| Format | How |
|---|---|
| JPEG | Lossless recompression; the original JPEG can be restored byte for byte (`--iqa` can change its XMP, see [Quality rating](#quality-rating)). CMYK JPEGs are converted to RGB and lossless (SOF3) JPEGs are decoded to pixels instead. |
| HEIC/HEIF, AVIF | Decoded at native bit depth. |
| WebP, PNG | Stills only. |
| TIFF / BigTIFF | Each page becomes `name_p1.jxl`, `name_p2.jxl`, ...; thumbnails and other reduced-resolution pages are skipped. |

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

## AVIF output

`-f avif` writes AVIF with [SVT-AV1](https://github.com/Uranite/svt-av1-tritium)
(the Tritium fork, tuned for still images) instead of JPEG XL. AVIF is always
lossy; `-q` defaults to 85 (SVT-AV1 CRF 23) and `-e` to 7 (preset 5), and
`-x KEY=VALUE` passes any other SvtAv1EncApp option (`-x preset=-1`,
`-x crf=20.5`). At the defaults a 16-thread CPU converts a library at about
10 s per 24 MP photo; a single photo takes about 25 s, as SVT-AV1 spreads one
still image over cores poorly. JPEGs are decoded to pixels (there is no
lossless JPEG recompression), and `--max-bit-depth` has no effect.

Viewers disagree on how AVIF color is described, so jxlall writes one of three
spaces, chosen by the source's gamut, in a form tested to display correctly in
Windows Photos, Chrome and Immich (Firefox and libvips-based tools read the
same signals):

| Source | Output |
|---|---|
| sRGB or untagged | sRGB: CICP 1/13/6 and an sRGB ICC profile |
| Display P3 | Display P3: CICP 12/13/6 and a "Display P3" ICC profile |
| Wider (Adobe RGB, ProPhoto, Rec.2020) | the smallest gamut holding both P3 and Adobe RGB, described by the ICC profile (CICP primaries unspecified) |

Rec.2020 primaries are avoided because Windows Photos shows them washed out,
and AVIF has no CICP code for Adobe RGB. Colors beyond the wide space
(ProPhoto, Rec.2020 extremes) are clipped. PQ and HLG sources keep their CICP
and pixels.

Other differences from JXL output:

- Always 10-bit 4:2:0 at full range: Windows cannot decode 4:4:4 AVIF.
- EXIF orientation is applied to the pixels and reset to 1, since viewers
  disagree on AVIF rotation.
- Gain maps are stored as an ISO 21496-1 `tmap` item with the base image as
  the fallback.
- Images larger than 16384x8704 (the encoder limit, e.g. 61 MP portraits)
  are stored as a grid of tiles.
- `-g` stores AV1 film grain from the same ISO model; the decoder
  synthesizes it.
- An EXIF `ColorSpace` of sRGB is set to Uncalibrated on wide-gamut output, or
  Immich would show an sRGB preview.

## Artifact removal

`--dejpeg` removes JPEG blocking and ringing with
[DeJPEGNet v2](https://github.com/Miosp/dejpeg) before encoding. It applies to
JPEG inputs; `--dejpeg=all` extends it to every input, except HDR, floating-point
and premultiplied-alpha images, which are encoded unchanged with a note.

Restored JPEGs are no longer recompressed losslessly, so the original JPEG cannot
be recovered from the output. No other setting changes: with the default `-q 100`,
restored images are stored losslessly and are usually much larger than the source,
so you will normally want `-q 90` or similar. Metadata, color profiles and alpha are
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

`a.jpg` becomes `a.jxl` (or `a.avif`). Existing files are never overwritten: a name clash
gets a suffix instead (`a.png.jxl`, `a.png.2.jxl`). With `--delete-originals` a source is deleted
only after its output has been synced to disk.

## Usage

`jxlall -h` lists the common options, `jxlall --help` all of them:

```
jxlall [OPTIONS] [INPUTS]...
Output:
  -o, --output <DIR>          output directory, mirrors the input tree (default: next to sources)
  -f, --format <jxl|avif>     output format (default jxl)
      --delete-originals      delete sources after successful conversion
Encoding:
  -q, --quality <0..=100>     same scale for both formats (default: 100 = lossless JXL, 85 AVIF)
  -e, --effort <1..=10>       higher is slower, smaller or better (default 7; 4 for lossless JXL)
  -g, --grain[=xN|ISO]        film grain from the metadata ISO, times N, or a fixed ISO
      --reencode-jpeg         re-encode JPEG pixels instead of recompressing losslessly
  -x <KEY=VALUE>              raw encoder setting, repeatable (--help only)
      --max-bit-depth <N>     store deeper images at N bits, 8..=16 (--help only)
AI:
      --dejpeg[=all]          remove JPEG artifacts first (all: every input)
      --iqa[=MODEL,MODEL]     store technical and aesthetic scores in XMP
      --stars[=T2,T3,T4,T5]   also write 1..=5 stars (implies --iqa)
      --overwrite-rating      replace an existing star rating (--help only)
      --ai-backend <B>        auto, tensorrt-rtx, webgpu or cpu (--help only)
Run:
  -r, --recursive             descend into subdirectories
  -p, --pattern <PAT>         extensions to scan, `|`-separated (--help only)
  -j, --jobs <N>              files converted at once (default: CPU count)
  -n, --dry-run               list what would be converted
      --fail-fast             stop at the first failure (--help only)
  -v, --verbose               log each file's details and timings
```

`-q` is one scale for both formats: equal values score about the same on
SSIMULACRA2. For JPEG XL it is libjxl's own (`cjxl -q`): 90 is distance 1,
100 lossless. For AVIF it maps to an SVT-AV1 CRF, matched against JPEG XL on
camera photos and film scans:

| `-q` | 100 | 97 | 95 | 90 | 85 | 80 | 70 | 60 | 50 |
|---|---|---|---|---|---|---|---|---|---|
| JPEG XL distance | lossless | 0.37 | 0.55 | 1.0 | 1.45 | 1.9 | 2.8 | 3.7 | 4.6 |
| AVIF CRF | 2.75 | 7 | 9.75 | 16.25 | 23 | 28.25 | 35 | 39 | 42 |

JPEGs are recompressed losslessly into JPEG XL whatever `-q` says, unless
`--reencode-jpeg`.

`-x` passes a setting straight to the encoder, after jxlall's own. For AVIF,
any SvtAv1EncApp option without the dashes. For JPEG XL, a libjxl frame
setting by its cjxl name: `epf`, `gaborish`, `resampling`, `dots`,
`patches`, `modular`, `responsive`, `progressive-ac`, `decoding-speed`,
`modular-predictor`, `brotli-effort`, `jpeg-recon-cfl` and a few more (an
unknown name lists them all). Unknown names and out-of-range values are
reported before anything is converted.

In a terminal, a progress bar shows files done, time elapsed and time left. The
estimate weighs files by size and starts once the first file has finished, so a
single photo shows its current step instead. A summary follows on stdout: sizes,
speed, per-format breakdown, the slowest file, notes and failures with their
reasons (the first 20; stderr has them all).

Directory scans skip hidden files and do not follow symlinks. A failed file
does not stop the run; with `--fail-fast` the first failure stops new
conversions and the rest are reported as skipped. Any failure, missing input or unreadable directory
makes the exit status non-zero.

`-g` is for photos that were denoised before conversion. It stores grain
parameters matching the original ISO (scaled for sensor size when EXIF has
the 35 mm focal length), and the decoder synthesizes the grain at a cost of
a few bytes. `-g=x1.5` makes it 1.5 times as strong. `-g=1800` uses one ISO
for every file instead, for photos whose EXIF was stripped; it is a
full-frame equivalent: multiply the camera ISO by the crop factor squared
(ISO 800 on a 1.5x crop sensor is about 1800). Losslessly recompressed JPEGs
get no grain; the summary counts them.

## Quality rating

`--iqa` scores each photo with two [Charm](https://github.com/FBehrad/Charm)
models (CVPR 2025, DINOv2-small, ONNX exports from
[Miosp/charm-onnx](https://huggingface.co/Miosp/charm-onnx)) and stores both
scores, 0 to 1, in XMP under `https://github.com/Miosp/jxlall/ns/iqa/1.0/`:

- `Technical`: detail and technical quality (`spaq`, the default, or
  `koniq10k`).
- `Aesthetic`: aesthetics and composition (`para`, the default, `aadb`,
  `tad66k` or `baid`).

Other models are picked by name, in any order: `--iqa=koniq10k,aadb`.

Each score is written with its model id; scores from different models are not
comparable. Charm sees the whole frame at its own aspect ratio, as displayed
(EXIF orientation applied), and keeps the 64 sharpest regions at full
resolution, so intentional blur such as bokeh is judged in context. Photos
are first downscaled to 1024 px; upstream Charm does that only for `spaq`
and `para`, so scores of the other models differ from upstream on larger
images.

`--stars` (which implies `--iqa`) also writes `xmp:Rating` (1 to 5 stars,
read by Lightroom, darktable, digiKam and Windows) from the mean of both
scores. `--stars=T2,T3,T4,T5` sets the minimum mean for 2, 3, 4 and 5 stars
(default 0.37, 0.48, 0.59, 0.64: on a set of curated camera photos and film
scans that gives about 10% one star, 20% two, 40% three, 20% four and 10% five). A rating the source
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
binary means following AGPL-3.0 terms. It also includes SVT-AV1
(BSD-3-Clause-Clear, with the Alliance for Open Media Patent License 1.0).

`--dejpeg` and `--iqa` download ONNX Runtime (MIT), their models (Apache-2.0) and,
when that backend is used, NVIDIA TensorRT RTX under its own license, from their
publishers at run time. None of them are part of the binary.
