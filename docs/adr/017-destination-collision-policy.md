# ADR 017: Never overwrite destination content

## Decision

Source namespaces avoid ordinary filename collisions. If the computed destination file already exists, CaptureOS compares full BLAKE3 hashes: identical bytes are reused as verified evidence; different bytes are a conflict. If a final name appears while a partial is being finalized, CaptureOS reports a conflict and retains the partial.

## Consequences

CaptureOS never replaces different destination bytes. A user can inspect the per-file error and retry after resolving the destination, rather than losing existing media.
