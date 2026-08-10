# ADR 021: RAW preview boundary

## Decision

Milestone 3 defines `RawPreviewProvider` behavior through the generic thumbnail provider boundary but does not claim RAW development. ARW, CR2, CR3, NEF, RAF, ORF, RW2, and DNG currently receive an explicit unsupported/placeholder state when no safe embedded-preview extractor is available.

## Consequences

RAW files remain indexed and their metadata/catalog records are still usable. CaptureOS never synthesizes a fake RAW preview and never decodes a full RAW merely to populate the grid. Future embedded-JPEG or platform RAW adapters can supply the same cache artifact model.
