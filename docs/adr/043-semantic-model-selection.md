# ADR 043: Opt-in SigLIP ONNX semantic-model selection

## Decision

Milestone 6 defines a provider boundary for a local image/text embedding model and names product
ID `google-siglip-base-patch16-224`, sourced from
[`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) at
immutable revision `7fd15f0689c79d79e38b1c2e2e2370a7bf2761ed`, as the controlled family for an
opt-in SigLIP ONNX pack. The provider accepts only static image/text
ONNX graphs and a matching tokenizer installed below CaptureOS’s controlled model root. It uses
local CPU inference through `tract-onnx` 0.21.17; no cloud endpoint, browser inference, Python
runtime, pickle payload, model-supplied script, or hosted GPU is part of the design.

The selected pack’s shared projection is 768 dimensions. Its image contract is an oriented RGB
224×224 input, Pillow-compatible bicubic resize (`resample: 3`), values scaled by `/255`, then
normalized with mean and standard deviation `0.5`. Its text contract is the checksum-verified
official static tokenizer JSON, including its normalizer, Unigram vocabulary, EOS-only template,
64-token limit, no BOS token, and EOS padding. Image and text outputs are L2-normalized only
within an admitted same-version embedding space.

This decision does **not** bundle source weights or an ONNX conversion. A first static conversion
is technically admitted only when its full manifest hash, source revision, source safetensors
hash, every file size/checksum, known file set, tokenizer regression vectors, fixed RGB24
reference raster, and image/text reference vectors match the compiled descriptor. Until then,
CaptureOS reports **Semantic model not installed** and continues to provide only truthful
deterministic search. The reference tolerance is fixed in CaptureOS code rather than supplied by
a pack. The source model’s Apache-2.0 metadata remains provenance rather than legal certification;
releases still require review of the exact source/conversion/distribution terms.

## Consequences

CaptureOS has one explicit provider shape for real local text-to-image and image-to-image
retrieval without hard-coding a particular vendor into query/UI code. Apple Silicon can use the
same native-Rust CPU path; future Windows support requires validation of the admitted graph’s
operation coverage and cannot be assumed from this ADR.

The provider remains replaceable. A later candidate needs an independent quality, runtime,
license, source, checksum, and reference-vector review; it must not silently compare vectors from
an incompatible model/version with the active SigLIP space.
