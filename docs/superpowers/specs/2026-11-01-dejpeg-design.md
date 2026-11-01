# DeJPEGNet v2 artifact removal (`--dejpeg`)

## Goal

Optional JPEG artifact removal in jxlall using `dejpeg-v2.onnx` from
[Miosp/dejpeg](https://github.com/Miosp/dejpeg) v2.0.0 (Apache-2.0). Off by
default. When on, it applies to JPEG inputs; an override extends it to all
inputs. The executable stays small: ONNX Runtime, GPU backends and models are
downloaded on first use, pinned by SHA-256.

This branch starts from `main`, independent of `iqa-rating-charm` (PR #4), which
serves only as a reference for download and CI patterns. The IQA work will later
be rewritten on top of the shared `src/ai` code introduced here, so that code stays
generic: nothing in `runtime` or `fetch` knows about dejpeg.

## CLI

| Flag | Meaning |
|---|---|
| `--dejpeg` | Remove artifacts from JPEG inputs |
| `--dejpeg-all` | With `--dejpeg`: remove artifacts from every input |
| `--dejpeg-backend <auto\|tensorrt-rtx\|directml\|webgpu\|cpu>` | Backend, default `auto` |

- `--dejpeg` overrides no other setting. `-d` keeps its default of 0, so restored
  images are stored losslessly unless `-d` is passed. The README says so.
- `--dejpeg-all` or `--dejpeg-backend` without `--dejpeg` warn that they have no
  effect, like `--noise-strength` without `--add-noise`.
- The "JPEGs recompressed losslessly get no grain" note for `--add-noise` is
  suppressed with `--dejpeg`, since those JPEGs take the pixel path.
- `--delete-originals` with `--dejpeg` is allowed and prints one warning that
  byte-exact JPEG recovery is lost.
- `--dry-run` downloads nothing.

The model is fixed to `dejpeg-v2`. There is no model flag and no texture variant.

## Backends

| Platform | `auto` order |
|---|---|
| Linux x86-64, arm64 (glibc) | TensorRT RTX, WebGPU, CPU |
| Windows x86-64, arm64 | TensorRT RTX, DirectML, WebGPU, CPU |
| macOS arm64 | WebGPU, CPU |
| Linux musl, macOS x86-64 | not available |

Musl binaries are fully static and cannot load shared libraries. Microsoft
publishes no x86-64 macOS core. On these builds `--dejpeg` reports that the build
has no AI support and stops before converting.

Naming an unavailable backend for the platform is a CLI error.

### Selection

- `auto` tries each backend in order, moving on with a note when a download,
  registration, session creation or a 64 px probe tile fails. CPU is last; if it
  fails the run stops.
- An explicit backend that fails stops the run.
- The chosen backend is always printed once.
- TensorRT RTX is only tried under `auto` when an NVIDIA GPU is detected, so other
  machines never fetch its 100+ MB package. Linux: a PCI device with vendor
  `0x10de` and class `0x03xxxx` in `/sys/bus/pci/devices`. Windows: `nvcuda.dll`
  in the system directory. An explicit `tensorrt-rtx` skips this check.
- A non-RTX NVIDIA card or an old driver costs one cached download, then falls
  through to the next backend.

### Artifacts

| Part | Platform | Source | Download |
|---|---|---|---|
| Core 1.31.1 | Linux x86-64 / arm64 | `microsoft/onnxruntime` release `.tgz` | 11 / 10 MB |
| Core 1.31.1 | macOS arm64 | `microsoft/onnxruntime` release `.tgz` | 37 MB |
| Core 1.24.4 with DirectML | Windows x86-64 / arm64 | NuGet `Microsoft.ML.OnnxRuntime.DirectML` | 12 MB |
| `DirectML.dll` 1.15.4 | Windows x86-64 / arm64 | NuGet `Microsoft.AI.DirectML` | 202 MB |
| WebGPU plugin 0.5.0 | all supported | PyPI `onnxruntime-ep-webgpu` wheel | 5-15 MB |
| TensorRT RTX plugin 0.5.0, CUDA 13 | Linux x86-64 / arm64 | `NVIDIA/TensorRT-RTX-EP-ABI` release | 147 / 135 MB |
| TensorRT RTX plugin 0.5.0, CUDA 13 | Windows x86-64 / arm64 | `NVIDIA/TensorRT-RTX-EP-ABI` release | 113 / 100 MB |
| `dejpeg-v2.onnx` | all supported | `Miosp/dejpeg` release v2.0.0 | 15.4 MB |

The DirectML NuGet package stops at 1.24.4, so Windows uses that core for every
backend. The WebGPU plugin's stated minimum core is also 1.24.4. CUDA 13 is the
only variant that covers both architectures on both systems, and TensorRT RTX
targets Ampere and newer RTX GPUs. Binaries are downloaded from their publishers,
not redistributed by jxlall.

Packaging facts that shape the loader:

- `DirectML.dll` is not in the Windows core's normal import table, so CPU and
  WebGPU never fetch its 202 MB package. Under `auto`, a Windows machine that
  reaches the DirectML step does fetch it, once, even when WebGPU would have worked.
- The Windows core imports the Visual C++ runtime (`MSVCP140.dll`,
  `VCRUNTIME140.dll`), which the README lists as a requirement.
- The Linux TensorRT RTX plugin has no RUNPATH, and its zip stores symlinks as small
  stub files. Real libraries are extracted under their SONAME and loaded by absolute
  path before the plugin, so its dependencies resolve. The same preload step loads
  `DirectML.dll` and `dxcompiler.dll` on Windows. The plugin also needs `libcuda.so.1`
  and `libnvidia-ml.so.1` from the NVIDIA driver; without them registration fails and
  `auto` moves on.

### Evidence

Measured on an i7-1260P with Iris Xe, with `dejpeg-v2.onnx`:

| Runtime | 512 px tile | 256 px tile |
|---|---|---|
| CPU, 16 threads, static core | 2.3 s | not run at 16 threads (8 threads: 0.43 s) |
| CPU, dynamic core 1.31.1 and 1.24.4, default threads | not run | about 0.40 s |
| WebGPU plugin, dynamic core 1.31.1 and 1.24.4 | about 0.85 s | not run on dynamic core |

The two cores give identical output means on each backend. A test binary with
`load-dynamic` is 0.7 MB, against 30 MB with the static core. tract cannot load
the model. TensorRT RTX, DirectML, macOS and Windows have not been run.

## Structure

- `src/ai/backend.rs`: backend names, order per platform, NVIDIA detection. No ONNX
  Runtime types.
- `src/ai/artifacts.rs`: pinned URLs, hashes and sizes per platform.
- `src/ai/runtime.rs`: loading the core once, preloading, registering plugins, the
  one shared ONNX Runtime environment, creating sessions.
- `src/ai/fetch.rs`: per-user cache with pinned downloads, adapted from PR #4's
  `src/iqa/fetch.rs`, with archive extraction added.
- `src/ai/tiling.rs`: tile plan, mirroring, feathered blending. Pure Rust, no ONNX
  Runtime types.
- `src/ai/dejpeg.rs`: model spec, pixel conversion, the restore loop.
- A stub replaces the ONNX Runtime side when the feature is off, as PR #4 does.

Cargo:

- `ort` with `load-dynamic` and `api-24` (the lowest core in use), no
  `download-binaries`.
- New optional dependencies: `ort`, `ureq`, `directories`, `sha2`, `zip`, `flate2`, `tar`.
- A default `ai` feature enables them. Musl and x86-64 macOS builds use
  `--no-default-features`.

## Download and cache

Rules, following PR #4's model cache and extending it to runtime libraries:

- Location: `<user cache>/jxlall/models` for models, `<user cache>/jxlall/runtime/<artifact>`
  for libraries.
- Each artifact pins URL, archive SHA-256 and size. Each extracted library pins its
  own SHA-256, rechecked on every load before it is loaded.
- Only the named files are extracted, each written under a fixed name, never using
  paths from the archive.
- Extraction goes to a temporary directory that is renamed into place, so an
  interrupted run leaves nothing usable behind.
- The core and preloaded libraries are loaded by absolute path, which avoids picking
  up a system `onnxruntime.dll` or an older inbox `DirectML.dll`.

The README states that `--dejpeg` downloads and runs native code from
Microsoft, and from NVIDIA where applicable, and that `--dejpeg-backend cpu`
limits this to the core.

## Pipeline

Inside `encode_all`, per decoded page:

1. Page is selected when it is `Decoded::Jpeg`, or any page under `--dejpeg-all`.
   Lossless (SOF3) JPEGs decode to `Decoded::Image`, so they are only selected by
   `--dejpeg-all`.
2. A selected JPEG skips jbrd and is pixel-decoded with `format::jpeg::decode`.
3. Eligible pixels are restored in place.
4. The image continues through `encode_decoded_image`, so `-d` and `--add-noise`
   behave as for any pixel input.

### Eligibility

Skipped with a note, encoded unchanged (reachable through `--dejpeg-all`):

- `Pixels::F32` frames.
- HDR frames: `hdr.intensity_target` set.
- Premultiplied alpha.

Unchanged by restoration: alpha, EXIF, XMP, ICC, orientation, gain map. `U8` and
`U16` samples are normalized to `[0, 1]` and written back in the same storage type
and bit depth. When every input pixel has R = G = B, the output channels are
replaced by their mean so the model cannot add color.

### Tiling

Ported from `packages/inference-core` of the dejpeg repository:

- Fixed grid, step = tile - overlap, overlap = max(8, tile / 8).
- Tiles that pass the image edge are mirrored, not padded.
- Cosine feather in overlaps, with weight 1 on image borders.
- Tile size is 512 px on GPU and 256 px on CPU. 256 px is faster per output pixel
  on the measured CPU. All sizes are multiples of the model's alignment of 16.
- One tile per run; no batching.

On a failed tile run, tile size halves down to a floor of 64 px and the image
restarts. The smaller size is kept for the rest of the run. At the floor, the file
fails.

### Concurrency

A mutex is held for the whole restore of one image, which also bounds the blend
buffers to one image (about 384 MB at 24 MP). Decode and encode stay parallel.

### Failure behavior

- Runtime or model load failure stops the run before any conversion.
- A failed restore fails that file: no output, source kept, counted in stats.
  Silently writing an unrestored image would misrepresent the result.

## Build and CI

ONNX Runtime is never linked, which also avoids the `LNK2038` `RuntimeLibrary`
mismatch between libjxl and the static core that fails PR #4's
`test (windows-2025)`.

The release matrix keeps its targets; musl and x86-64 macOS build with
`--no-default-features`, and lint also covers that configuration. Archives gain no
files. `cargo-deny` is updated for any new license. The smoke test does not cover
`--dejpeg`, as it needs downloads; the ignored tests below do.

## Testing

Unit tests, with a fake inference function where a model would run:

- Tile plan covers every pixel; origins stay on the step grid.
- Blend weights are positive across the image and identity inference returns the
  input within rounding, including small images and edge overhangs.
- Mirror index.
- Selection matrix: format, `--dejpeg-all`, F32, HDR, premultiplied.
- Grayscale neutrality.
- `U8` and `U16` round trip, including left-aligned 10-bit and 12-bit.
- Tile-size halving on failure.
- Backend order per platform, explicit-versus-auto failure, NVIDIA detection parsing.
- Hash mismatch on a cached library triggers re-download and never loads it.

Ignored tests (download required, `cargo test -- --ignored`):

- Real model on a compressed test image: PSNR against the original improves.

Manual, on real hardware, before release:

- TensorRT RTX on an NVIDIA RTX machine.
- DirectML, WebGPU and TensorRT RTX on Windows, on core 1.24.4. A plugin that fails
  to register on this core falls through under `auto`, and the failure is recorded in
  the README's platform notes.
- WebGPU on macOS arm64 and Linux arm64.

## Out of scope

Any change to `--iqa` or PR #4. CUDA, OpenVINO, CoreML and MIGraphX backends. A model
flag and the texture model. Tile batching. Changing `-d` defaults. Musl and x86-64
macOS AI support.
