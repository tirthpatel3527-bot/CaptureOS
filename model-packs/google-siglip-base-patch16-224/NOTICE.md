# CaptureOS SigLIP model-pack provenance

This directory contains metadata and development tooling for the optional local CaptureOS
semantic-model pack. It does **not** contain model weights, converted ONNX artifacts, customer
media, embeddings, or a downloaded model binary.

The source candidate is [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224)
at immutable revision `7fd15f0689c79d79e38b1c2e2e2370a7bf2761ed`. The Hugging Face model metadata
labels the repository artifact `Apache-2.0`; the source repository did not include a standalone
license file in the audited file manifest. That metadata is recorded as provenance, not legal
advice or a commercial redistribution certification. A release must separately review the
exact source weights, converted artifacts, and intended distribution.

The only admitted source weight is `model.safetensors`, SHA-256
`2c63cb7d1f2e95ba501893cbb8faeb4ea9a3af295498d35097126228659c2af8`, 812,672,320 bytes.
CaptureOS never downloads or loads `pytorch_model.bin`, uses `trust_remote_code`, or runs code
from the model repository. The development converter uses a fixed local source directory,
checksum-verifies every listed source file, exports static ONNX encoders, verifies parity, and
records each derived artifact checksum before atomically publishing the local pack.

The local product runtime is Rust plus `tract-onnx`, CPU-only. It requires only static ONNX,
the checksum-verified official `tokenizer.json`, and static pack data. It makes no network call
while indexing, querying, or using Find Similar.
