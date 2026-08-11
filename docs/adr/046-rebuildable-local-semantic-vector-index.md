# ADR 046: Rebuildable local semantic vector index

## Decision

Magic Search treats the persistent local vector index as a project-scoped, versioned derived
artifact. Durable SQLite embedding records are the source of truth. The `VectorIndex` boundary
supports an exact small-project baseline and a replaceable local scalable implementation; it must
not require a hosted vector database or send vectors off-device.

An index identity includes project scope, semantic model/version, embedding/preprocessing version,
dimension, and index implementation/version. It contains only compatible READY embeddings. If the
index is missing, stale, or corrupt, CaptureOS rebuilds it from stored embeddings; if embeddings
are also absent the explicit local indexing job queues per-asset embedding work where a usable
managed preview/source exists. No semantic pairwise CaptureGraph edges are created for every
nearest neighbor.

The current `captureos-m6-lsh.v1` implementation is exact at or below 4,096 vectors. Larger
projects use four deterministic 16-bit sign-LSH tables. Planes are generated once per rebuild and
once per loaded index, not for every vector/query; lookup probes an exact signature and its
one-bit neighbors, fetches no more than 8,192 project-scoped IDs, then re-ranks those vectors
locally. This is intentionally an acceleration artifact, not a semantic-recall guarantee.

## Consequences

Deleting an index cannot delete a project, media card, FileInstance, human decision, review
session, Similar Set, or analysis artifact. Current-project isolation is enforced before
retrieval, so an embedding from another project cannot become a search candidate.

Generated 768-dimensional measurements currently retain exact self queries but show low
candidate coverage of exact top-K neighbors at 10k/50k. This limitation is reported by
MagicSearchBench and must not be presented as real semantic quality. The boundary is intentionally
separate from SQLite metadata persistence so a future locally reviewed ANN implementation can
improve recall/scale without changing query-planner, ranking, or UI contracts.
