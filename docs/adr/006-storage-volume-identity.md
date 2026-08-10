# ADR 006: Storage volume identity

## Decision

Give each volume a CaptureOS ID, preserve optional filesystem identity, and record mount state/location separately.

## Consequences

`/Volumes/SamsungT7` is never the sole key for a volume. Disconnected media remains meaningful in the catalog.
