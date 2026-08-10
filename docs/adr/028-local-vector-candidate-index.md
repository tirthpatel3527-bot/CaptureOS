# ADR 028: Local candidate index for visual grouping

## Decision

M4 keeps compact descriptors and LSH-style pHash bucket keys in SQLite and uses them only to generate bounded candidate pairs. Four pHash bands and same-camera/time windows improve recall; each bucket is capped at 96 members, and oversize buckets are skipped. A project with at most 256 eligible inputs also receives a bounded complete candidate pass, which repairs recall when related frames share no exact pHash band. Candidate pairs must still pass the unchanged visual and relevant camera/time gates before unioning into a group.

`SimilarityGroup` membership is persisted directly, with grouping method/version and reproducible IDs. The descriptor provider is separate from the candidate/index strategy. No cloud vector database, specialized vector extension, or catalog-wide all-pairs `SIMILAR_TO` graph is introduced.

## Consequences

The baseline is local, persistent, versioned, and rebuildable. The deliberately small complete pass is bounded to at most 32,640 pair checks; larger catalogs avoid naive full-catalog O(n²) comparison. It is deliberately a first candidate layer rather than an approximate-nearest-neighbor performance guarantee. A future local ANN/vector index can consume the same versioned descriptor records after independent licensing, maintenance, cross-platform, and benchmark review.
