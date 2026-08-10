# ADR 022: Offline original media

## Decision

Availability belongs to `FileInstance`, not `MediaAsset`. The visual browser selects a preferred available instance when one exists, otherwise shows the logical asset as `OFFLINE ORIGINAL` while retaining metadata and valid cache artifacts.

## Consequences

Disconnecting a source volume does not discard project structure, cached thumbnails, or metadata. The inspector shows every copy and its availability. Cache cleanup still cannot affect offline or mounted customer media.
