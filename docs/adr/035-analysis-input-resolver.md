# ADR 035: Analysis input resolver and dedicated cache artifact

## Decision

Milestone 4.1 introduces `AnalysisInputResolver` in `capture-core`. It resolves the single
`ANALYSIS_IMAGE` required by local analysis in this order:

1. a ready, source-fingerprint-matched `analysis_preview` artifact;
2. a ready, source-fingerprint-matched 1600px browsing `PREVIEW` artifact;
3. a deterministic catalog-marked available `FileInstance`, read only to generate a contained
   `analysis_preview`; then
4. another available `FileInstance` for the same `MediaAsset`.

The dedicated `analysis_preview` has a configurable 2048px target long edge, its own generator
version, and a cache identity based on MediaAsset, FileInstance, source fingerprint, artifact
type, generator version, and size class. The image adapter preserves aspect ratio/orientation and
does not upscale a smaller source. An analyzer is given only the validated cache path; it is never
given a general original-media path.

`NEEDS_ORIGINAL` now means exactly that no sufficient cache exists and every known physical copy
is offline or unavailable. A malformed available input is `CORRUPT`, a decoder limitation is
`UNSUPPORTED`, and an unexpected attempt failure is `FAILED`. Optional face-provider
unavailability remains component-level `NOT_APPLICABLE` and does not invalidate ready technical
evidence.

No new table is required: `preview_artifacts` already stores typed, versioned artifact rows and
the existing unique key includes `(media_asset_id, source_file_instance_id, artifact_type,
size_class)`. The M4.1 analysis settings version intentionally changes so existing current
evidence is rebuilt once against the corrected input contract, while historical records remain
intact and foreign-key-linked decisions are preserved.

## Consequences

Supported locally available JPEGs no longer require a user to manually prepare an AI-specific
preview before analysis. Cache hits avoid source decoding and no successful browsing preview is
regenerated. Source files remain read-only, and generated artifacts remain removable solely by
CaptureOS cache cleanup. The resolver is generic over the repository and thumbnail-provider
boundaries so later approved analyzers can declare a different minimum input requirement without
coupling themselves to a camera, platform, or storage vendor.
