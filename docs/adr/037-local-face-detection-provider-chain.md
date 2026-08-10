# ADR 037: Local face-detection provider chain

## Decision

Milestone 4.3 replaces the macOS JXA face bridge with a direct Objective-C FFI adapter for
`VNDetectFaceRectanglesRequest`. The adapter receives only a CaptureOS-managed analysis-preview
file URL, executes inside an autorelease pool, returns structured JSON over a C-owned string
boundary, and serializes the Vision request. It performs one retry only for explicit transient
XPC/connection errors.

CaptureOS uses a stable `captureos-local-face-detection` cache identity. Apple Vision is the
first provider on macOS. If it fails, CaptureOS uses the bundled, static Ultra-Light-Fast-Generic-
Face-Detector-1MB RFB-320 ONNX model through `tract-onnx` 0.21.17, a pure-Rust local runtime.
The fallback records the Apple failure only as developer provenance; a usable fallback rectangle
result is `READY`.

The bundled model is `models/ultraface-rfb-320.onnx`, sourced from the upstream
[Ultra-Light-Fast-Generic-Face-Detector-1MB repository](https://github.com/linzaer/ultra-light-fast-generic-face-detector-1mb)
file `models/onnx/version-RFB-320.onnx`, version `version-RFB-320`, size 1,270,727 bytes, SHA-256
`34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017`. Upstream repository and
model license are MIT (Copyright 2019 linzai). CaptureOS ships no identity, recognition,
embedding, demographic, or cloud face capability. The model's fixed 320×240 RGB input, 0.70
confidence threshold, 0.30 IoU NMS, and 0.2%-image minimum rectangle area are documented in the
source and tested as local rectangle processing.

## Consequences

Face rectangles work on supported macOS even when the host Vision service fails and can run on
other supported desktop platforms through the local fallback. The model is a fixed application
asset: there is no model registry download, update check, telemetry, cloud call, or user media
upload. Face cache settings are independently versioned, so upgrading this provider requeues only
face artifacts from ready managed previews; technical evidence, descriptors, similarity groups,
recommendations, source files, and previews remain untouched.

Landmarks and eye state are deliberately component-local. This milestone does not claim that the
rectangle-only fallback can infer eye state. A future landmark provider needs an explicit review
of privacy, licensing, accuracy, and platform behavior.
