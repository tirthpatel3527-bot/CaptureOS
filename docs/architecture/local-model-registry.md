# Local model registry

Milestone 4 establishes a persisted local-model registry contract without introducing a model downloader, hosted inference service, or automatic network contact. The deterministic Capture Intelligence baseline requires no external model. Milestone 4.3 adds one reviewed, fixed application asset for local face rectangles. Milestone 6 extends the same gate to an opt-in local image/text embedding pack for Magic Search; it does not relax the admission requirements.

## Registry record

Migration 008 creates `local_model_registry`; additive Migration 011 adds its semantic model
metadata and the related semantic embedding/index/history tables. Its domain record is
`LocalModelRecord`:

| Field | Purpose |
| --- | --- |
| `model_id` | Stable product-level model identifier, separate from a local file name. |
| `provider` / `version` | Provider boundary and exact implementation version. |
| `model_family` | Stable architecture family such as `siglip`; metadata only, never executable dispatch. |
| `local_relative_path` | Optional path relative to a controlled CaptureOS model-storage root; never a trusted arbitrary path. |
| `checksum` | Integrity identity for the installed artifact when applicable. |
| `capability` | Narrow task such as visual embedding, face detection, or eye landmarks. |
| `input_size` / `embedding_dimension` | Versioned image raster size and shared image/text vector dimension when applicable. |
| `status` | Availability/admission state, including an unavailable or rejected state. |
| `license` / `license_url` / `source_url` | License metadata and reviewable origin; a URL is documentation, not permission for automatic download. |
| `file_size_bytes` / `hardware_requirements` | Honest disk and local-runtime requirements. |
| `registered_at` | Audit timestamp. |

For semantic retrieval, the registry additionally records the model family, image input size, embedding dimension, preprocessing/tokenizer version, explicit installed-status, and the set of narrow capabilities (image embedding and/or text embedding). An image/text provider is usable only when both encoders belong to the same registered embedding space and version.

The table preserves registry metadata only. It does not promise that an arbitrary record can run, approve an external license, or make a model pack available merely because a file exists on disk.

## Admission gate for a future model

Before a model becomes available in a CaptureOS release or a future explicit model manager, all of the following must be documented and accepted:

1. The task has a local provider boundary and a meaningful unavailable/error state.
2. The model, runtime, conversion tooling, and test dataset have commercially compatible license terms for the intended distribution.
3. The exact version, source, expected checksum, size, hardware requirements, and runtime compatibility are recorded.
4. Installation is an explicit user action. CaptureOS must not contact a remote server or download a model simply because a view opened or analysis started.
5. The installed path is canonicalized under the owned model-storage root; traversal, symlink escape, and arbitrary executable/script paths are rejected.
6. A supported static format/runtime is used where possible. Model-supplied executable code, hooks, or scripts are never run as part of installation or inference.
7. Results carry provider/model/settings provenance, input fingerprint, confidence, status, and error through the appropriate analysis or semantic artifact record.
8. A model can be disabled, removed from use, or rebuilt without deleting customer originals or erasing prior evidence.

Large experimental model binaries do not belong in Git. A small fixed release asset may be committed only after the same license/integrity review, with its exact source, checksum, size, and runtime documented; it must never be a customer-specific machine assumption.

## Current provider inventory

| Provider / version | Capability | Source and license posture | File size / hardware | Registry status |
| --- | --- | --- | --- | --- |
| `captureos-deterministic-image` / `m4.det.v1` | Perceptual descriptors and technical evidence | CaptureOS Rust source tree; repository MIT license. No external model binary or network dependency. | 0 external model bytes; bounded local CPU analysis. | Built in; model-free |
| `captureos-technical-recommendation` / `m4.rules.v1` | Evidence-based technical labels | CaptureOS Rust source tree; repository MIT license. No external model. | 0 external model bytes; local CPU only. | Built in; model-free |
| `captureos-local-face-detection` / `m4.face-detection-chain.v3` | Anonymous face rectangles, with face-region sharpness measured separately | Apple Vision rectangle request on macOS first; static fallback is [UltraFace RFB-320](https://github.com/linzaer/ultra-light-fast-generic-face-detector-1mb), MIT, Copyright 2019 linzai. No cloud or download path. | `ultraface-rfb-320.onnx`, 1,270,727 bytes, SHA-256 `34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017`; pure-Rust `tract-onnx` CPU inference. | Built in; ADR 037 |
| `google-siglip-base-patch16-224` / `tract-onnx` / `captureos.semantic.siglip-base-p16-224.v1` | Shared image/text embeddings for Magic Search | Controlled source: [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) at pinned revision `7fd15f0689c79d79e38b1c2e2e2370a7bf2761ed`. Its upstream metadata labels the repository artifact Apache-2.0; CaptureOS does **not** bundle or declare the conversion commercially approved. The compiled descriptor pins source and final artifact checksums. | Static ONNX image encoder + text encoder + tokenizer only; 224px RGB input and 768-dimensional projection. The reviewed v6 local pack is 815,600,873 bytes. CPU inference uses `tract-onnx` 0.21.17 (dual MIT/Apache-2.0). Apple Silicon runs native Rust CPU code; future Windows support remains contingent on on-target admission. | No binary ships in this repository; unavailable until explicit local admission succeeds; ADRs 043–045 |

The Apple Vision attempt is intentionally not an exception to the registry license gate: because CaptureOS does not distribute it, it is not a redistributable model record. The UltraFace fallback passed the gate as a fixed, reviewed application asset; its scope is rectangles only. A future landmark model must separately meet the full gate.

## Magic Search pack admission

Magic Search admits exactly one compiled, reviewable SigLIP pack descriptor, not arbitrary folders or
third-party ONNX conversions. An operator explicitly runs the documented development installer;
it verifies the pinned source, converts only safetensors to static ONNX in a temporary controlled
staging directory, verifies its output, and atomically publishes the fixed pack directory. The
desktop app never downloads the approximately 813 MB upstream source weight or a converted ONNX
pack simply because a project opens.

For `google/siglip-base-patch16-224`, the controlled contract is a 768-dimensional shared
projection, oriented RGB 224×224 input, Pillow-compatible bicubic resize, channel values scaled
by `/255` and normalized with mean/std `0.5`, and the static official tokenizer JSON with its
64-token EOS-only contract. The image and text vectors are L2-normalized only after successful
inference. These values are descriptor-pinned provider configuration, not an assertion that every
third-party conversion is compatible.

Before the registry marks the controlled pack `AVAILABLE`, CaptureOS validates canonical
containment below the model root, rejects symlink/path escape, unexpected files/directories, and
unknown manifest fields, verifies the compiled manifest digest and configured checksums/sizes,
runs tokenizer regression checks, checksum-verifies a fixed RGB24 reference raster, and compares
image/text outputs to vectors generated from the pinned source revision. A mismatch, unsupported
ONNX operation, tokenizer mismatch, missing file, or unreviewed license keeps the provider
unavailable. The local `tract-onnx` runtime is open source under
[MIT or Apache-2.0](https://github.com/sonos/tract), but its license does not establish rights
for model weights or conversion artifacts. The installation/removal commands are documented in
[Controlled local SigLIP pack installation](semantic-model-install.md).

The upstream model card’s Apache-2.0 label is source provenance, not legal advice or a blanket approval of all conversion artifacts, datasets, or downstream uses. Releases must re-review the exact model files and their redistribution terms before bundling anything.

## Security and privacy consequences

Model metadata and files are untrusted external inputs. A checksum tells CaptureOS about a known artifact; it does not make an incompatible license or unsafe runtime acceptable. Registry records must not become telemetry, cloud credentials, or a way to export customer media. Face providers remain detection/landmark-only until a separately approved privacy design authorizes anything more.

See [Capture Intelligence architecture](capture-intelligence.md), [Magic Search architecture](magic-search.md), and [Capture Intelligence security](../security/capture-intelligence.md).
