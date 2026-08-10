# ADR 023: Logical-media pagination

## Decision

The default media browser queries bounded pages of logical `MediaAsset` rows and picks one preferred `FileInstance` for each. The frontend requests 120 records at a time and offers incremental loading; the physical-file table remains an advanced view.

## Consequences

CaptureOS does not send an entire large catalog to the frontend or create a card per physical copy. SQL performs filtering, deterministic filename/camera/lens search, sorting, and availability selection. Viewer navigation is intentionally limited to the loaded page until a future virtualized viewport scheduler is added.
