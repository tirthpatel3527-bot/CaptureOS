# ADR 065: Local-folder export reuses verified-copy safety and private reports

## Decision

M9 ships only a `LocalFolder` production destination. It is intentionally implemented on the
existing ingest streaming `copy_and_verify` primitive: copy to a CaptureOS partial, hash while
streaming with BLAKE3, re-read/hash, reject a changed source or verification mismatch, and
atomically finalize only without overwriting a different destination file. An already-existing
content-identical regular file is recorded as `SKIPPED_IDENTICAL`; a different file, unsafe
destination link, unsafe source link, insufficient space, unavailable source, or destination
inside a selected source root blocks/fails honestly.

The planner deterministically orders by capture time, Moment ordinal, and stable asset identity.
It sanitizes every untrusted label/name/template component and rejects traversal. The pure naming
engine accepts only bounded literal tokens (`{project}`, `{moment}`, `{sequence}`, `{original}`,
`{capture_date}`, `{camera}`); it cannot execute code. Selection prefers an available source
`FileInstance` with independently verified-copy evidence, checksum evidence, newer observation,
then stable identity.

Cancellation and startup recovery mark an execution interrupted/cancelled, retain valid verified
files, and permit a later job for the same immutable manifest. The preflight recomputes source
availability, capacity, destination collision equivalence, and containment immediately before
write.

Each terminal execution emits JSON and text Delivery Reports locally and persists the same
immutable private-safe summary. Client reports contain a non-internal delivery reference, plan
status, counts, verified bytes,
manifest checksum, local-folder destination type, and verification policy. They exclude source
paths, internal asset IDs, notes, AI/technical scores, Studio recommendations, embeddings, and
model data.

## Consequences

M9 has no cloud, hosted storage, Adobe/NLE integration, render pipeline, RAW development,
write-back, or automatic delivery. Future `ProductionAdapter` implementations must preserve the
Plan → Manifest → Job contract and the safety/verification/privacy policy, but may add a
separately approved adapter-specific transport only after explicit review.
