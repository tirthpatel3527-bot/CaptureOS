# ADR 044: Semantic-model licensing and explicit local installation

## Decision

Magic Search model files are never bundled, auto-downloaded, or silently converted in Milestone
6. Its product-level ID is `google-siglip-base-patch16-224`; the candidate source is the upstream
[`google/siglip-base-patch16-224` model card](https://huggingface.co/google/siglip-base-patch16-224),
which labels the code and weights Apache-2.0. The referenced upstream `model.safetensors` file is
813 MB with SHA-256
`2c63cb7d1f2e95ba501893cbb8faeb4ea9a3af295498d35097126228659c2af8`.

That reference does not authorize CaptureOS to distribute every conversion or downstream asset.
An ONNX pack is installed only by an explicit user/developer action and records its immutable
upstream revision, source-weight provenance, conversion provenance, model/tokenizer paths,
individual checksums, file sizes, model family, input size, embedding dimension, preprocessing
version, source URL, and license reference in the local model registry. CaptureOS rejects path
traversal, symlink escape, executable hooks, arbitrary scripts, Python/pickle formats, and a
missing or configured-checksum mismatch. The local runtime itself,
[`tract`](https://github.com/sonos/tract), is dual MIT/Apache-2.0; that runtime license is not a
license for model weights.

The provider remains `NOT_INSTALLED` or `PENDING_REFERENCE_VALIDATION` until it validates
versioned image and text reference vectors from the registered immutable source. This is a
technical compatibility gate as well as a license/provenance gate.

## Consequences

Opening a project, starting an embedding job, or searching never initiates a multi-gigabyte
download or contacts a remote service. A pack with uncertain redistribution terms remains local
development material rather than a CaptureOS release asset. Model-card licensing and provenance
are documentation, not legal advice; a release still requires review of the exact files, their
conversion path, and intended commercial distribution.

The manual installation tradeoff is deliberate: a photographer can keep using local metadata and
technical filters while semantic retrieval is unavailable instead of receiving fabricated results
or an unreviewed binary.
