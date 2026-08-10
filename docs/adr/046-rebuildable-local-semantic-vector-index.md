# ADR 046: Rebuildable local semantic vector index

## Decision

Magic Search treats the persistent local vector index as a project-scoped, versioned derived
artifact. Durable SQLite embedding records are the source of truth. The `VectorIndex` boundary
supports an exact small-project baseline and a replaceable local scalable implementation; it must
not require a hosted vector database or send vectors off-device.

An index identity includes project scope, semantic model/version, embedding/preprocessing version,
dimension, and index implementation/version. It contains only compatible READY embeddings. If the
index is missing, stale, or corrupt, CaptureOS rebuilds it from stored embeddings; if embeddings
are also absent it queues local re-analysis where a usable managed preview/source exists. No
semantic pairwise CaptureGraph edges are created for every nearest neighbor.

## Consequences

Deleting an index cannot delete a project, media card, FileInstance, human decision, review
session, Similar Set, or analysis artifact. Current-project isolation is enforced before
retrieval, so an embedding from another project cannot become a search candidate.

The baseline may use exact retrieval only where its benchmarked scale is acceptable. The boundary
is intentionally separate from SQLite metadata persistence so a future local ANN implementation
can improve 10k/50k/100k+ behavior without changing query-planner, ranking, or UI contracts.
