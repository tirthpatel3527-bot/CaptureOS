# ADR 026: Local Capture Intelligence architecture

## Decision

Milestone 4 adds Capture Intelligence as a local provider layer behind `capture-intelligence` contracts for decoding, image analysis, similarity, face detection, and recommendations. `capture-core` owns durable orchestration; `persistence` owns versioned evidence; the desktop owns commands/progress/UI. `AnalysisInputResolver` gives analysis a validated CaptureOS-managed artifact rather than a general original-media path. It may read a catalog-marked available source only to create a contained, separately versioned analysis-preview cache artifact.

Every result is an `AnalysisArtifact` with provider/model/settings provenance, input fingerprint, timestamp, confidence, status, payload, and error. Related fingerprints, groups, technical evidence, face evidence, recommendations, and append-only human decisions are persisted separately. The baseline is deterministic and model-free; optional platform providers remain adapters.

## Consequences

Capture Intelligence can work offline and cannot silently become a cloud feature. A provider may be unavailable without invalidating unrelated deterministic evidence. The design supports future local model replacement, but does not authorize semantic search, identity recognition, artistic ranking, video/audio intelligence, automatic culling, or Milestone 5 work.
