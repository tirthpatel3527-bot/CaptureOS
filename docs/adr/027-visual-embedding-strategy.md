# ADR 027: Deterministic visual descriptor strategy

## Decision

M4 uses a deterministic descriptor: pHash, dHash, a 4×4×4 color signature, and a mean-centred 8×8 luminance descriptor quantized to 64 signed bytes. The compact descriptor is stored as a SQLite BLOB; pHash/color/descriptor similarity are combined as inspectable visual evidence.

The descriptor is intentionally not called a semantic embedding. Exact duplicate claims require a verified full BLAKE3 content hash, never filename, size, metadata, or a bounded fast fingerprint. Near-duplicate/similarity evidence is separately labelled with distance/confidence.

## Consequences

The baseline has no model download, opaque neural score, or giant JSON float array. It is useful for re-encodes, resizes, and related-frame candidates but has known limits for crops, scene changes, and semantic similarity. A future provider can replace the descriptor without changing the `AnalysisArtifact` or grouping contracts.
