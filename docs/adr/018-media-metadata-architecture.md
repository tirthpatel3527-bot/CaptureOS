# ADR 018: Read-only normalized media metadata

## Decision

Milestone 3 stores a compact normalized metadata record per `MediaAsset`, tied to the `FileInstance` and source fingerprint that produced it. The record includes useful photo, video, and audio fields plus an extensible JSON payload for provider-specific raw fields. Original timestamps retain their raw/local value and source; CaptureOS does not invent a timezone or overwrite the original `MediaAsset` timestamp.

## Consequences

`MetadataExtractor` providers are local adapters. The current macOS adapter uses safe argument-array calls to `sips` and `mdls`; WAV is parsed locally. Missing fields stay null. There is no metadata write-back, XMP write, camera API, cloud lookup, or universal manufacturer-tag schema.
