# ADR 019: CaptureOS-managed preview cache

## Decision

Generated thumbnails, previews, posters, and on-demand analysis previews live only below the CaptureOS application-data `preview-cache` root. SQLite stores a relative cache path plus asset/file-instance IDs, source fingerprint, provider, generator version, size class, status, and failure reason. An `analysis_preview` is separately versioned from browsing renditions so a changed analysis contract does not invalidate the grid cache.

## Consequences

Cache identity changes when its source evidence or generator version changes. Cache cleanup removes only the dedicated cache root and artifact rows; it never receives a customer source path. The catalog and metadata survive cache cleanup and artifacts regenerate on demand. Browsing previews are not color-critical editing renders.
