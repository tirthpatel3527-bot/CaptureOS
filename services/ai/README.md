# Local intelligence service boundary

Milestone 4 keeps product intelligence in the Rust `capture-intelligence` crate, behind local provider contracts for image analysis, similarity, faces, and recommendations. The shipped baseline uses deterministic visual/technical methods and contains no Python runtime, PyTorch, ONNX Runtime, model binary, hosted endpoint, telemetry, GPU service, or automatic download.

`services/ai/` remains a future experimentation boundary. A future local model provider must enter through the same provenance and artifact contracts, work offline after explicit installation, preserve confidence and uncertainty, keep customer media on-device, and never turn an unverified inference into a human-confirmed fact. It must pass the [local model registry](../../docs/architecture/local-model-registry.md) license/integrity gate before it can be bundled or offered to users.

The optional macOS Vision face-landmark adapter is not a service or a CaptureOS-distributed model. It uses a host-OS capability only when present and records an honest unavailable/not-applicable result otherwise.
