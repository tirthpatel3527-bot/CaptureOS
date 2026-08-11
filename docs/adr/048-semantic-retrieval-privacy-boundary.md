# ADR 048: Semantic retrieval privacy boundary

## Decision

Magic Search runs entirely on the local device. Customer originals, managed previews, search text,
embeddings, model metadata, vector-index artifacts, search history, face count evidence, and
result explanations are not uploaded, telemetered, synchronized, or exported automatically. An
embedding is potentially sensitive derived information and receives the same local-only handling
as other Capture Intelligence artifacts.

The semantic provider receives only canonicalized CaptureOS-owned analysis-preview paths. Model
and index paths are canonicalized below controlled roots; static model files are checksum-validated
when configured and cannot execute arbitrary Python, pickle data, or provider scripts. Query
history is stored per project locally and can be cleared without affecting catalog data.

Milestone 6 contains no face identity recognition, cross-project person matching, face clustering,
demographic inference, creative/emotional judgment, video semantic search, or audio semantic
search. Face count is permissible only as existing anonymous deterministic evidence.

## Consequences

Semantic functionality remains usable offline once a local model pack and compatible embeddings
exist. A source volume being offline does not make a cached embedding leave the device or cease to
be searchable. The local index can be rebuilt through the Magic Search indexing control without
changing original media, catalog/project identity, Capture Intelligence history, or photographer
decisions. Milestone 6 does not expose a clear-index/delete-derived-data control.
