# ADR 013: BLAKE3 verification evidence

## Decision

CaptureOS uses BLAKE3 for ingest verification. It hashes source bytes while streaming the copy, re-reads the destination partial with BLAKE3, and marks a copy verified only when the two full cryptographic digests agree and the source size/modification observation remained stable.

## Consequences

The Milestone 1 fast fingerprint is never verification evidence. Verified hashes are persisted on the logical `MediaAsset` and the per-destination ingest item, enabling safe equality reuse on a later retry.
