# ADR 043: Opt-in SigLIP ONNX semantic-model selection

## Decision

Milestone 6 defines a provider boundary for a local image/text embedding model and names product
ID `google-siglip-base-patch16-224`, sourced from
[`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224), as the
candidate family for an opt-in SigLIP ONNX pack. The provider accepts only static image/text
ONNX graphs and a matching tokenizer installed below CaptureOS’s controlled model root. It uses
local CPU inference through `tract-onnx` 0.21.17; no cloud endpoint, browser inference, Python
runtime, pickle payload, model-supplied script, or hosted GPU is part of the design.

The candidate’s expected shared projection is 768 dimensions. Its expected image contract is an
oriented RGB 224×224 input, values scaled by `/255`, then normalized with mean and standard
deviation `0.5`. Its expected text contract lowercases input, uses the matching model tokenizer,
and pads/truncates to 64 tokens. Image and text outputs are L2-normalized only within an admitted
same-version embedding space.

This decision does **not** bundle the source weights, an ONNX conversion, tokenizer files, or a
runtime-validated model pack. Until a pack passes the separate registry, checksum, and reference
vector checks, CaptureOS reports semantic retrieval as unavailable and continues to provide only
truthful deterministic search.

## Consequences

CaptureOS has one explicit provider shape for real local text-to-image and image-to-image
retrieval without hard-coding a particular vendor into query/UI code. Apple Silicon can use the
same native-Rust CPU path; future Windows support requires validation of the admitted graph’s
operation coverage and cannot be assumed from this ADR.

The provider remains replaceable. A later candidate needs an independent quality, runtime,
license, source, checksum, and reference-vector review; it must not silently compare vectors from
an incompatible model/version with the active SigLIP space.
