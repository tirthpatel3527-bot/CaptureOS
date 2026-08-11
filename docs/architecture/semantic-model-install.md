# Controlled local SigLIP pack installation

Magic Search uses one optional, local-only model pack in Milestone 6.1:
`captureos.semantic.siglip-base-p16-224.v1`.

The desktop application does not download a model at startup, when a project opens, or when
indexing/search begins. The current development installation path is an explicit operator command.
It produces static ONNX artifacts below the CaptureOS application-data model root; no weight or
ONNX binary is committed to Git.

## Reviewed source

| Item | Value |
| --- | --- |
| Upstream repository | [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) |
| Immutable revision | `7fd15f0689c79d79e38b1c2e2e2370a7bf2761ed` |
| Source artifact | `model.safetensors` only; `pytorch_model.bin` is never loaded |
| Source size | 812,672,320 bytes |
| Source SHA-256 | `2c63cb7d1f2e95ba501893cbb8faeb4ea9a3af295498d35097126228659c2af8` |
| License metadata | `Apache-2.0` on the upstream Hugging Face model page |
| Runtime | `tract-onnx` 0.21.17, local CPU only |
| Model input/output | RGB 224×224 images and 64-token text inputs → 768-dimensional pooled features |

The source repository’s audited file listing did not include a standalone license file. CaptureOS
records its Apache-2.0 metadata, the supplied Apache text, and a provenance notice, but this is
not legal advice or a commercial redistribution certification. Do not bundle this pack in a
release without a separate review of the source terms and intended distribution.

## What is admitted

The source lock in
[`model-packs/google-siglip-base-patch16-224/source-lock.json`](../../model-packs/google-siglip-base-patch16-224/source-lock.json)
pins every source file required for conversion. The compiled CaptureOS provider additionally pins
the exact manifest SHA-256 and every final artifact’s path, size, and SHA-256 through
[`approved-pack.json`](../../model-packs/google-siglip-base-patch16-224/approved-pack.json).
The generated manifest also records the exact preprocessing identity
`siglip-rgb224-pillow-bicubic-scale05.v1` and the intentionally narrow capabilities
`image_text_embedding`; it does not authorize detection, identity recognition, video/audio
analysis, or any broader AI capability.

The initial reviewed static pack is 815,600,873 bytes on disk:

| Local artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `image-encoder.onnx` | 371,718,169 | `ee67818e4f1c62bf765442ed6d511a8b080d59b0872e401fcc5d9d2fb6306ef2` |
| `text-encoder.onnx` | 441,247,152 | `3d36877d59f3da9879f24c688febd42627f7da8b252ca77e7c3ba7c0b0bb03c2` |
| `tokenizer.json` | 2,399,357 | `c6e405cb7c670d56636a9402c81023a55bc6c3c53d89cf02b92f5c5005bfe920` |
| `reference.rgb24` | 175,911 | `9a2ef30a40740598108109f4fbca048bfd6eae2041bab61144999e299dccdf10` |

The remaining small manifest, Apache notice, and conversion-provenance files are also
checksum-pinned. The provider rejects an unexpected file, directory, symlink, manifest field,
source revision, tokenizer behavior, artifact size/checksum, ONNX output contract, or reference
vector mismatch. A self-consistent arbitrary ONNX folder is not an admitted pack.

## Explicit development installation on macOS

This command is the only step that may use the network, and only because the operator explicitly
started it. It downloads the source files at the fixed immutable revision, validates their
checksums, uses a temporary pinned Python 3.12 converter, validates PyTorch→ONNX Runtime parity,
and atomically publishes the static pack only if all checks pass.

```sh
APP_DATA="$HOME/Library/Application Support/com.captureos.desktop"
BUILDER_ROOT="/private/tmp/captureos-siglip-builder"

/Library/Frameworks/Python.framework/Versions/3.12/bin/python3 -m venv "$BUILDER_ROOT"
"$BUILDER_ROOT/bin/pip" install --requirement scripts/m6/requirements-siglip-pack.txt
"$BUILDER_ROOT/bin/python" scripts/m6/build_siglip_pack.py \
  --download-source /private/tmp/captureos-siglip-source \
  --model-root "$APP_DATA/semantic-models"
```

The command identifies its one operator-requested network download and then emits separate
local-only stage messages for source verification, static ONNX export, CPU ONNX Runtime parity,
and atomic publish. Those messages include the pinned source, license metadata, and model/pack
sizes, followed by final destination, installed bytes, and final artifact/manifest checksums. Its
final destination is fixed beneath:

```text
$APP_DATA/semantic-models/google-siglip-base-patch16-224
```

It never overwrites an existing model pack. A failed conversion remains in an
`.google-siglip-base-patch16-224.installing-*` staging directory and is not usable by CaptureOS;
the operator may inspect or remove that staging directory explicitly. The converter loads the
local `model.safetensors` only, uses `local_files_only=True` and `trust_remote_code=False`, and
does not execute repository scripts or a pickle weight.

For an already staged source directory, replace `--download-source …` with
`--source-dir /path/to/verified/source`. This second form performs no network operation.

After the command reports `PACK_READY`, restart the desktop application or reload the project.
Magic Search changes from **Semantic model not installed** to a ready local-model state only after
the Rust provider independently validates the compiled descriptor, all files, tokenizer vectors,
both ONNX graphs, and reference image/text vectors through `tract-onnx`.

## Preprocessing and retrieval contract

The installed pack uses the official SigLIP contract:

- Decode the CaptureOS-managed preview as RGB, resize to 224×224 using Pillow-compatible bicubic
  sampling (`resample: 3`), divide by 255, then normalize every channel with mean/std 0.5.
- Parse the checksum-verified official `tokenizer.json` with its static normalizer, Metaspace,
  SentencePiece Unigram vocabulary and EOS template. Text is lowercased/normalized by that
  artifact, truncates to 64 IDs, has no BOS token, and pads with EOS ID 1.
- Both exported graphs return the reviewed pooled 768-wide image/text features. CaptureOS
  L2-normalizes them before cosine-style local retrieval. A similarity value is a ranking signal,
  not an object detector, identity claim, or calibrated probability.

The static converter proves PyTorch→ONNX Runtime parity before publishing. Runtime admission also
loads and evaluates the final graph through `tract-onnx`; unsupported operators or a numerical
mismatch make the model unavailable rather than falling back to a fake provider.

## Offline behavior and safe removal

After a pack has been admitted, indexing, text-query embedding, Magic Search, and Find Similar
use only local static files, CaptureOS-managed preview pixels, the local SQLite catalog, durable
embeddings, and rebuildable indexes. They make no request to Google, Hugging Face, OpenAI, or
another service. Disabling the network after installation must not change those capabilities.

The following is a dry-run by default; it validates only the fixed pack path and reports all
retained data:

```sh
APP_DATA="$HOME/Library/Application Support/com.captureos.desktop"
python3 scripts/m6/remove_siglip_pack.py --model-root "$APP_DATA/semantic-models"
```

Only an explicit confirmation removes the fixed controlled model directory:

```sh
python3 scripts/m6/remove_siglip_pack.py \
  --model-root "$APP_DATA/semantic-models" \
  --confirm-remove-local-model
```

Removal does not alter projects, originals, previews, Capture Intelligence evidence, Similar
Sets, culling decisions, ratings, notes, representatives, review sessions, durable semantic
embeddings, or semantic-index records. Without a compatible local pack, new text/image query
embeddings cannot be made, so semantic text search and Find Similar correctly become unavailable
until the same approved pack is reinstalled; deterministic metadata/technical filters remain
available.

## Platform scope

The admitted conversion and runtime validation are currently performed on macOS Apple Silicon
with local CPU inference. The provider boundary does not hard-code macOS, but Windows requires
its own on-target conversion/runtime validation and descriptor review before this exact pack is
called supported there. No GPU, CoreML, cloud, telemetry, or background download is claimed.
