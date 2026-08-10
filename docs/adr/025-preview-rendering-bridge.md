# ADR 025: Preview rendering bridge

## Decision

The desktop UI receives opaque, runtime-only `captureos-preview://localhost/<artifact-uuid>`
URLs for visual media. SQLite stores only `PreviewArtifact` identity and a CaptureOS-cache
relative path; it never stores a webview URL or an absolute cache/original-media path.

The Tauri custom protocol accepts only `GET` requests for a single UUID. It resolves that UUID
only when it is a registered `READY` preview artifact, then canonicalizes the database-relative
path below the CaptureOS-owned preview-cache root. Absolute paths, traversal, missing files, and
symlinks that escape that root are rejected. The bridge serves only JPEG or PNG files whose magic
bytes match their extension. It has no scope for source roots, user folders, volumes, or any other
local path.

Preparation progress is the result of processing logical `MediaAsset` records, not physical
`FileInstance` copies. Its terminal counts are serialized in the completed visual-preparation
job's existing resume metadata. The desktop reloads this recorded result after restart and does
not replace it with an empty no-work invocation. For an existing pre-M3.1 catalog lacking that
serialized result, CaptureOS reconciles only terminal PreviewArtifact/metadata evidence for the
currently selected FileInstance and persists the recovered logical result once. It never uses a
raw catalog count as a substitute for preparation work.

## Consequences

Grid cards use SMALL at small density and MEDIUM otherwise, the inspector uses MEDIUM, and the
viewer prefers PREVIEW before falling back to smaller cache artifacts. All remain displayable when
the original is offline because the bridge depends only on the registered generated cache file.
Clearing the cache deletes only generated artifacts and their records, invalidates displayed URLs,
and triggers normal logical-media regeneration. A failed image request is replaced in the UI by a
CaptureOS media fallback rather than a browser broken-image icon.

A logical media card is `READY` whenever any resolved SMALL, MEDIUM, or PREVIEW artifact is
usable. Failed larger renditions and provider-attempt diagnostics remain in artifact records for
developer investigation, but they cannot override a usable cached poster or thumbnail in the
user-visible status.
