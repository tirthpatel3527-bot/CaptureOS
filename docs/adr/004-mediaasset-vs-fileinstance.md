# ADR 004: MediaAsset vs FileInstance

## Decision

Model a conceptual media item (`MediaAsset`) separately from each physical location (`FileInstance`).

## Consequences

Backup copies, disconnectable volumes, renamed copies, and future deduplication can be represented without treating a filename or path as identity.
