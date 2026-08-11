#!/usr/bin/env python3
"""Explicit, development-only builder for the controlled CaptureOS SigLIP M6.1 pack.

This script is deliberately not part of the CaptureOS desktop runtime. It only accepts the
single source lock committed with this repository, uses a locally staged safetensors checkout,
and emits two static fixed-shape ONNX graphs. It never enables ``trust_remote_code``, never
loads a pickle weight, and never copies model weights into Git.

Run this only after an operator explicitly chose to install the optional local model. The desktop
app itself has no downloader and never invokes this script.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
import shutil
import sys
import time
import uuid
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
PACK_METADATA_DIRECTORY = REPOSITORY_ROOT / "model-packs" / "google-siglip-base-patch16-224"
SOURCE_LOCK_PATH = PACK_METADATA_DIRECTORY / "source-lock.json"
APACHE_LICENSE_PATH = PACK_METADATA_DIRECTORY / "Apache-2.0.txt"

PACK_DIRECTORY = "google-siglip-base-patch16-224"
PACK_ID = "captureos.semantic.siglip-base-p16-224.v1"
RUNTIME = "tract-onnx"
INPUT_SIZE = 224
EMBEDDING_DIMENSION = 768
TEXT_SEQUENCE_LENGTH = 64
ONNX_OPSET = 17
REFERENCE_WIDTH = 307
REFERENCE_HEIGHT = 191
REFERENCE_TEXT = "a small red boat near the water"
PREPROCESSING_VERSION = "siglip-rgb224-pillow-bicubic-scale05.v1"
SUPPORTED_CAPABILITIES = ("image_text_embedding",)
TOKENIZER_REGRESSION_TEXTS = (
    "woman in red",
    "Two people on a boat!",
    "  PORTRAIT\tby water  ",
    "yellow boat, bright sun",
)
MAX_ONNX_ABSOLUTE_ERROR = 0.002


class BuildError(RuntimeError):
    """A source, conversion, or admission verification failed."""


def stage(message: str) -> None:
    """Emit a concise, operator-facing conversion stage without treating it as pack data."""

    print(f"STAGE: {message}", flush=True)


def locked_source_summary(source_lock: dict[str, Any]) -> str:
    """Return only committed source-lock details for local installer progress messages."""

    upstream = source_lock["upstream"]
    weights = source_lock["files"]["model.safetensors"]
    return (
        f"source={upstream['repository']}@{upstream['revision']}; "
        f"license-metadata={upstream['licenseMetadata']}; "
        f"model.safetensors={weights['bytes']:,} bytes"
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_source_lock() -> dict[str, Any]:
    try:
        return json.loads(SOURCE_LOCK_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BuildError(f"cannot read committed source lock: {error}") from error


def assert_python_toolchain() -> None:
    if sys.version_info[:2] != (3, 12):
        raise BuildError(
            "this reviewed converter requires Python 3.12; use the pinned temporary venv "
            "documented in scripts/m6/requirements-siglip-pack.txt"
        )
    expected = {
        "huggingface-hub": "0.24.6",
        "numpy": "1.26.4",
        "torch": "2.4.1",
        "transformers": "4.44.2",
        "safetensors": "0.4.5",
        "onnx": "1.16.2",
        "onnxruntime": "1.19.2",
        "Pillow": "10.4.0",
        "tokenizers": "0.19.1",
        "sentencepiece": "0.2.0",
    }
    mismatches = []
    for package, version in expected.items():
        try:
            installed = importlib.metadata.version(package)
        except importlib.metadata.PackageNotFoundError:
            installed = "not installed"
        if installed != version:
            mismatches.append(f"{package}={installed} (expected {version})")
    if mismatches:
        raise BuildError("pinned converter toolchain mismatch: " + "; ".join(mismatches))


def verify_source_directory(source: Path, source_lock: dict[str, Any]) -> Path:
    stage(
        "source verification (local-only; no network): validating committed checksums and sizes for "
        + locked_source_summary(source_lock)
    )
    resolved = source.resolve(strict=True)
    if not resolved.is_dir():
        raise BuildError("source path is not a directory")
    for name, expected in source_lock["files"].items():
        path = resolved / name
        if path.is_symlink() or not path.is_file():
            raise BuildError(f"required source file is absent or not a regular file: {name}")
        actual_bytes = path.stat().st_size
        actual_hash = sha256_file(path)
        if actual_bytes != expected["bytes"] or actual_hash != expected["sha256"]:
            raise BuildError(
                f"source verification failed for {name}: {actual_bytes} bytes / {actual_hash}; "
                f"expected {expected['bytes']} bytes / {expected['sha256']}"
            )
    unexpected_pickle = resolved / "pytorch_model.bin"
    if unexpected_pickle.exists():
        # It may coexist in a complete upstream checkout, but this builder never reads it.
        print("INFO: ignoring untrusted pickle weight pytorch_model.bin", file=sys.stderr)
    stage("source verification complete (local-only; no network)")
    return resolved


def download_pinned_source(source: Path, source_lock: dict[str, Any]) -> Path:
    """Download only the committed allowlist after explicit --download-source invocation."""

    from huggingface_hub import snapshot_download

    upstream = source_lock["upstream"]
    stage(
        "source download (operator-requested network operation): fetching only the committed allowlist for "
        + locked_source_summary(source_lock)
    )
    source.mkdir(parents=True, exist_ok=True)
    snapshot_download(
        repo_id=upstream["repository"],
        revision=upstream["revision"],
        allow_patterns=list(source_lock["files"].keys()),
        local_dir=str(source),
        local_dir_use_symlinks=False,
    )
    return source


def deterministic_reference_rgb() -> bytes:
    """Non-photographic raw RGB fixture that exercises resize + channel normalization."""

    payload = bytearray(REFERENCE_WIDTH * REFERENCE_HEIGHT * 3)
    offset = 0
    for y in range(REFERENCE_HEIGHT):
        for x in range(REFERENCE_WIDTH):
            payload[offset] = (x * 29 + y * 11 + (x * y) % 251) % 256
            payload[offset + 1] = (x * 7 + y * 37 + (x + 3 * y) % 241) % 256
            payload[offset + 2] = (x * 17 + y * 19 + (x * 5 + y * 13) % 239) % 256
            offset += 3
    return bytes(payload)


def normalized(values: Any) -> Any:
    import numpy as np

    array = np.asarray(values, dtype=np.float32).reshape(-1)
    magnitude = float(np.linalg.norm(array))
    if not np.isfinite(magnitude) or magnitude <= np.finfo(np.float32).eps:
        raise BuildError("model emitted a non-finite or zero-norm embedding")
    return array / magnitude


def model_file(path: Path, staging: Path) -> dict[str, Any]:
    relative = path.relative_to(staging).as_posix()
    return {"path": relative, "bytes": path.stat().st_size, "sha256": sha256_file(path)}


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    # The staging directory is private to this invocation; a replace avoids a half-written manifest.
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def build_pack(source: Path, model_root: Path, source_lock: dict[str, Any]) -> tuple[Path, float, float, float]:
    import numpy as np
    import onnxruntime as ort
    import torch
    from PIL import Image

    root = model_root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    root = root.resolve(strict=True)
    final = root / PACK_DIRECTORY
    if final.exists() or final.is_symlink():
        raise BuildError(
            f"refusing to overwrite an existing local model pack: {final}; remove it explicitly first"
        )
    staging = root / f".{PACK_DIRECTORY}.installing-{uuid.uuid4().hex}"
    staging.mkdir(mode=0o700)
    if staging.resolve().parent != root:
        raise BuildError("controlled staging directory escaped the selected model root")

    # Keep Transformers fully offline after the source directory has been validated.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    # Caches must never be published with the model pack. Keep any library migration/cache
    # metadata outside the staging directory, alongside other explicit development artifacts.
    converter_cache = root.parent / ".captureos-siglip-converter-cache"
    os.environ["HF_HOME"] = str(converter_cache / "hf")
    from transformers import AutoImageProcessor, AutoTokenizer, SiglipModel

    stage(
        "ONNX export (local-only; no network): loading the checksum-verified safetensors source and "
        "exporting fixed image/text graphs; "
        + locked_source_summary(source_lock)
    )
    started = time.perf_counter()
    try:
        model = SiglipModel.from_pretrained(
            str(source),
            local_files_only=True,
            trust_remote_code=False,
            use_safetensors=True,
            torch_dtype=torch.float32,
        )
        model.eval()
        load_ms = (time.perf_counter() - started) * 1000

        tokenizer = AutoTokenizer.from_pretrained(
            str(source), local_files_only=True, trust_remote_code=False, use_fast=False
        )
        image_processor = AutoImageProcessor.from_pretrained(
            str(source), local_files_only=True, trust_remote_code=False
        )
        if tokenizer.eos_token_id != 1 or tokenizer.pad_token_id != 1:
            raise BuildError(
                f"unexpected official SigLIP pad/EOS contract: eos={tokenizer.eos_token_id}, "
                f"pad={tokenizer.pad_token_id}"
            )

        reference_rgb = deterministic_reference_rgb()
        reference_path = staging / "reference.rgb24"
        reference_path.write_bytes(reference_rgb)
        reference_image = Image.frombytes("RGB", (REFERENCE_WIDTH, REFERENCE_HEIGHT), reference_rgb)
        pixel_values = image_processor(images=reference_image, return_tensors="pt")["pixel_values"]
        if tuple(pixel_values.shape) != (1, 3, INPUT_SIZE, INPUT_SIZE):
            raise BuildError(f"official image processor produced unexpected shape {tuple(pixel_values.shape)}")

        def token_ids(text: str) -> torch.Tensor:
            encoded = tokenizer(
                text,
                padding="max_length",
                truncation=True,
                max_length=TEXT_SEQUENCE_LENGTH,
                return_tensors="pt",
            )
            if set(encoded) != {"input_ids"}:
                raise BuildError(f"official tokenizer unexpectedly produced inputs {sorted(encoded)}")
            ids = encoded["input_ids"]
            if tuple(ids.shape) != (1, TEXT_SEQUENCE_LENGTH):
                raise BuildError(f"official tokenizer produced unexpected shape {tuple(ids.shape)}")
            return ids

        reference_ids = token_ids(REFERENCE_TEXT)

        class ImageEncoder(torch.nn.Module):
            def __init__(self, source_model: SiglipModel) -> None:
                super().__init__()
                self.source_model = source_model

            def forward(self, pixel_values: torch.Tensor) -> torch.Tensor:
                return self.source_model.get_image_features(pixel_values=pixel_values)

        class TextEncoder(torch.nn.Module):
            def __init__(self, source_model: SiglipModel) -> None:
                super().__init__()
                self.source_model = source_model

            def forward(self, input_ids: torch.Tensor) -> torch.Tensor:
                return self.source_model.get_text_features(input_ids=input_ids)

        image_encoder = ImageEncoder(model).eval()
        text_encoder = TextEncoder(model).eval()
        with torch.inference_mode():
            torch_image = image_encoder(pixel_values).cpu().numpy()
            torch_text = text_encoder(reference_ids).cpu().numpy()
        if tuple(torch_image.shape) != (1, EMBEDDING_DIMENSION) or tuple(torch_text.shape) != (
            1,
            EMBEDDING_DIMENSION,
        ):
            raise BuildError("SigLIP pooled image/text feature shapes did not match the reviewed 768D contract")

        image_onnx = staging / "image-encoder.onnx"
        text_onnx = staging / "text-encoder.onnx"
        torch.onnx.export(
            image_encoder,
            (pixel_values,),
            str(image_onnx),
            input_names=["pixel_values"],
            output_names=["embedding"],
            opset_version=ONNX_OPSET,
            do_constant_folding=True,
            dynamo=False,
        )
        torch.onnx.export(
            text_encoder,
            (reference_ids,),
            str(text_onnx),
            input_names=["input_ids"],
            output_names=["embedding"],
            opset_version=ONNX_OPSET,
            do_constant_folding=True,
            dynamo=False,
        )

        stage(
            "ONNX Runtime parity (local-only; no network): evaluating the fixed reference image and text "
            "against PyTorch and CPU ONNX Runtime; "
            + locked_source_summary(source_lock)
        )
        ort_image = ort.InferenceSession(str(image_onnx), providers=["CPUExecutionProvider"])
        ort_text = ort.InferenceSession(str(text_onnx), providers=["CPUExecutionProvider"])
        onnx_image = ort_image.run(None, {"pixel_values": pixel_values.cpu().numpy()})[0]
        onnx_text = ort_text.run(None, {"input_ids": reference_ids.cpu().numpy()})[0]
        image_error = float(np.max(np.abs(torch_image - onnx_image)))
        text_error = float(np.max(np.abs(torch_text - onnx_text)))
        if image_error > MAX_ONNX_ABSOLUTE_ERROR or text_error > MAX_ONNX_ABSOLUTE_ERROR:
            raise BuildError(
                "PyTorch-to-ONNX Runtime parity failed: "
                f"image={image_error:.8f}, text={text_error:.8f}, limit={MAX_ONNX_ABSOLUTE_ERROR}"
            )

        tokenizer_path = staging / "tokenizer.json"
        shutil.copyfile(source / "tokenizer.json", tokenizer_path)
        license_path = staging / "Apache-2.0.txt"
        shutil.copyfile(APACHE_LICENSE_PATH, license_path)
        notice_path = staging / "NOTICE.txt"
        upstream = source_lock["upstream"]
        notice_path.write_text(
            "CaptureOS local SigLIP pack provenance\n\n"
            f"Product pack: {PACK_ID}\n"
            f"Upstream: {upstream['sourceUrl']}\n"
            f"Immutable revision: {upstream['revision']}\n"
            f"Source license metadata: {upstream['licenseMetadata']}\n"
            f"Source safetensors SHA-256: {source_lock['files']['model.safetensors']['sha256']}\n"
            "This static ONNX conversion was created by CaptureOS's explicitly invoked, "
            "pinned development converter. No source model-repository code, pickle weight, or "
            "runtime network service was used. The source metadata is not legal advice or a "
            "commercial redistribution certification.\n",
            encoding="utf-8",
        )

        package_versions = {
            package: importlib.metadata.version(package)
            for package in (
                "huggingface-hub",
                "numpy",
                "torch",
                "transformers",
                "safetensors",
                "onnx",
                "onnxruntime",
                "Pillow",
                "tokenizers",
                "sentencepiece",
            )
        }
        # Keep the emitted provenance deterministic. Timing/parity observations are printed for
        # the explicit installation record but never become pack identity, so the same reviewed
        # source/toolchain can reproduce the same descriptor on another supported machine.
        conversion_provenance = {
            "converter": "scripts/m6/build_siglip_pack.py",
            "python": ".".join(map(str, sys.version_info[:3])),
            "packages": package_versions,
            "onnxOpset": ONNX_OPSET,
            "pytorchToOnnxRuntimeMaxAbsoluteErrorLimit": MAX_ONNX_ABSOLUTE_ERROR,
            "sourceWeightsLoaded": "model.safetensors only",
            "trustRemoteCode": False,
            "networkDuringConversion": False,
        }
        conversion_provenance_path = staging / "conversion-provenance.json"
        atomic_json(conversion_provenance_path, conversion_provenance)
        manifest: dict[str, Any] = {
            "formatVersion": 2,
            "packId": PACK_ID,
            "modelId": "google-siglip-base-patch16-224",
            "modelFamily": "siglip",
            "runtime": RUNTIME,
            "modelVersion": f"{PACK_ID}+{upstream['revision']}",
            "sourceUrl": upstream["sourceUrl"],
            "sourceRevision": upstream["revision"],
            "sourceWeightsSha256": source_lock["files"]["model.safetensors"]["sha256"],
            "license": upstream["licenseMetadata"],
            "licenseUrl": upstream["licenseUrl"],
            "preprocessingVersion": PREPROCESSING_VERSION,
            "supportedCapabilities": list(SUPPORTED_CAPABILITIES),
            "imageModel": model_file(image_onnx, staging),
            "textModel": model_file(text_onnx, staging),
            "tokenizer": model_file(tokenizer_path, staging),
            "licenseFile": model_file(license_path, staging),
            "noticeFile": model_file(notice_path, staging),
            "conversionProvenanceFile": model_file(conversion_provenance_path, staging),
            "inputSize": INPUT_SIZE,
            "embeddingDimension": EMBEDDING_DIMENSION,
            "textSequenceLength": TEXT_SEQUENCE_LENGTH,
            "tokenIds": {"eos": 1, "pad": 1},
            "textInputs": ["input_ids"],
            "imageOutputIndex": 0,
            "textOutputIndex": 0,
            "textPooling": "pooled",
            "tokenizerSelfTests": [
                {
                    "text": text,
                    "expectedIds": token_ids(text).reshape(-1).tolist(),
                }
                for text in TOKENIZER_REGRESSION_TEXTS
            ],
            "referenceVectors": {
                "formatVersion": 2,
                "image": {
                    "width": REFERENCE_WIDTH,
                    "height": REFERENCE_HEIGHT,
                    "rgb24": model_file(reference_path, staging),
                },
                "textQuery": REFERENCE_TEXT,
                "expectedImageEmbedding": normalized(torch_image).tolist(),
                "expectedTextEmbedding": normalized(torch_text).tolist(),
            },
        }
        atomic_json(staging / "captureos-semantic-model.json", manifest)
        installed_bytes = sum(path.stat().st_size for path in staging.iterdir() if path.is_file())
        stage(
            "publish (local-only; no network): atomically publishing the checksum-verified static pack "
            f"({installed_bytes:,} bytes); {locked_source_summary(source_lock)}"
        )
        os.replace(staging, final)
        return final, load_ms, image_error, text_error
    except Exception:
        # Never publish an incomplete final directory. The controlled staging directory is kept
        # for operator diagnostics and can only be removed explicitly by the operator.
        print(f"ERROR: pack build failed; retained non-admitted staging directory: {staging}", file=sys.stderr)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--model-root",
        required=True,
        type=Path,
        help="CaptureOS-owned semantic-models directory; the fixed pack name is appended automatically.",
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument(
        "--source-dir",
        type=Path,
        help="Previously staged, pinned source directory to verify and convert offline.",
    )
    source.add_argument(
        "--download-source",
        type=Path,
        metavar="STAGING_DIR",
        help="Explicitly download only the committed source allowlist to this staging directory.",
    )
    arguments = parser.parse_args()

    try:
        assert_python_toolchain()
        source_lock = read_source_lock()
        source_dir = arguments.source_dir
        if arguments.download_source is not None:
            source_dir = download_pinned_source(arguments.download_source, source_lock)
        assert source_dir is not None
        source_dir = verify_source_directory(source_dir, source_lock)
        final, load_ms, image_error, text_error = build_pack(source_dir, arguments.model_root, source_lock)
        manifest = json.loads((final / "captureos-semantic-model.json").read_text(encoding="utf-8"))
        print("PACK_READY")
        print(f"destination={final}")
        print(f"installed_bytes={sum(path.stat().st_size for path in final.iterdir() if path.is_file())}")
        print(f"manifest_sha256={sha256_file(final / 'captureos-semantic-model.json')}")
        print(f"image_onnx_sha256={manifest['imageModel']['sha256']}")
        print(f"text_onnx_sha256={manifest['textModel']['sha256']}")
        print(f"tokenizer_sha256={manifest['tokenizer']['sha256']}")
        print(f"pytorch_model_load_ms={load_ms:.3f}")
        print(f"pytorch_to_onnx_runtime_image_max_absolute_error={image_error:.9f}")
        print(f"pytorch_to_onnx_runtime_text_max_absolute_error={text_error:.9f}")
        return 0
    except BuildError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    except Exception as error:  # pragma: no cover - protects a user from a falsely ready pack
        print(f"ERROR: unexpected conversion failure: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
