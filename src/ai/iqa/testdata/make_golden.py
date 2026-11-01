"""Reference tokens and scores for the Rust Charm tokenizer tests.

Uses the NumPy tokenizer of https://github.com/Miosp/charm-onnx and the
exported models (numpy, pillow and onnxruntime needed):

    PYTHONPATH=/path/to/charm-onnx/src python3 make_golden.py > charm_golden.txt

Images and the position table used for the token sums are synthetic so
the Rust test can rebuild them without fixtures; keep `pattern` and
`synthetic_pos_table` in sync with `tokenizer::tests`. Scores use the real
tokens (real table) and models.
"""

import dataclasses
import tempfile
import urllib.request
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

from charm_onnx.specs import DATASETS
from charm_onnx.tokenizer import CharmTokenizer

REVISION = "cbcd14f8e00381f4227d2c48a0ffaa532b6a7555"
BASE_URL = f"https://huggingface.co/Miosp/charm-onnx/resolve/{REVISION}"
CACHE = Path(tempfile.gettempdir()) / f"charm-golden-{REVISION[:7]}"
# Single scale, multi scale without resize (pad), and with the >1024 px resize
# in landscape and portrait.
SIZES = [(451, 300), (1000, 872), (1999, 1333), (601, 1203)]
MODELS = ["spaq", "koniq10k", "para", "aadb", "tad66k", "baid"]  # token sums use the first
POS_SIDE, POS_DIM = 37, 384  # DINOv2-small table shape
# jxlall caps every model's input at 1024 px; upstream only spaq and para.
MAX_IMAGE_SIDE = 1024


def pattern(width: int, height: int) -> np.ndarray:
    """Gradients with hashed noise whose strength varies by area (uint8 RGB)."""
    y, x = np.mgrid[0:height, 0:width].astype(np.uint64)
    out = np.empty((height, width, 3), dtype=np.uint8)
    for c in range(3):
        mixed = ((x * np.uint64(73856093)) ^ (y * np.uint64(19349663)) ^ np.uint64(c * 83492791)) & np.uint64(0xFFFFFFFF)
        noise = ((mixed * np.uint64(2654435761)) & np.uint64(0xFFFFFFFF)) >> np.uint64(24)
        base = (x * 255 // (width - 1) + y * 255 // (height - 1) + np.uint64(c * 85)) % 256
        amp = (x // 53 + y // 71) % 7
        out[..., c] = (base * (8 - amp) + noise * amp) // 8
    return out


def synthetic_pos_table() -> np.ndarray:
    rows = np.arange(1 + POS_SIDE * POS_SIDE, dtype=np.float64)[:, None]
    dims = np.arange(POS_DIM, dtype=np.float64)[None, :]
    return np.sin(rows * 0.013 + dims * 0.071).astype(np.float32)[None]


def fetch(dataset: str, name: str) -> Path:
    path = CACHE / dataset / name
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(f"{BASE_URL}/{dataset}/{name}", path)
    return path


def floats(values) -> str:
    return " ".join(f"{v:.6f}" for v in values)


def main() -> None:
    specs = {m: dataclasses.replace(DATASETS[m], max_image_side=MAX_IMAGE_SIDE) for m in MODELS}
    tokenizers = {m: CharmTokenizer(np.load(fetch(m, "pos_embed.npy")), specs[m]) for m in MODELS}
    synthetic = CharmTokenizer(synthetic_pos_table(), specs[MODELS[0]])
    sessions = {m: ort.InferenceSession(fetch(m, "model.onnx"), providers=["CPUExecutionProvider"]) for m in MODELS}
    with tempfile.TemporaryDirectory() as tmp:
        for width, height in SIZES:
            path = Path(tmp) / f"{width}x{height}.png"
            Image.fromarray(pattern(width, height)).save(path)
            patches, pos_embed, scale_ids = tokenizers[MODELS[0]](path)
            print(f"case {width} {height}")
            print(f"tokens {len(scale_ids)}")
            print("scale_ids " + "".join(str(int(s)) for s in scale_ids))
            print("patch_sums " + floats(patches.astype(np.float64).sum(axis=(1, 2, 3))))
            print("pos_sums " + floats(synthetic(path)[1].astype(np.float64).sum(axis=1)))
            for m in MODELS:
                p, pe, ids = tokenizers[m](path)
                feeds = {"patches": p[None], "pos_embed": pe[None], "scale_ids": ids[None]}
                score = sessions[m].run(["score"], feeds)[0][0]
                print(f"score {m} {score:.6f}")


if __name__ == "__main__":
    main()
