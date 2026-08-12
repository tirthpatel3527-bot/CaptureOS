# Delivery Brain local security and privacy boundary

M9 is local-only. It does not upload originals, cached previews, plan names, human decisions,
ratings, notes, Moment labels, manifest records, checksums, source paths, destination paths,
embeddings, Studio artifacts, or reports. It requires no account, cloud credential, paid API,
telemetry, remote queue, or hosted storage.

## Filesystem safety

- The photographer chooses an existing local destination folder; planning writes nothing.
- Source and destination paths are canonicalized and contained within their approved roots.
  Absolute/traversal relative paths and symlink source/destination targets are rejected.
- Untrusted filenames, project names, Moment labels, camera strings, and custom-template values
  pass deterministic component sanitization before a destination path is built.
- The destination cannot be inside a selected source root. This prevents export from changing the
  indexed source inventory.
- Different existing destination content is a collision, never an overwrite. An existing file is
  reused only after current BLAKE3 source/destination equivalence.
- The copier writes a CaptureOS partial and verifies source/destination BLAKE3 hashes before an
  atomic no-overwrite finalization. A cancelled, disconnected, failed, or interrupted operation
  does not falsely treat a partial as final and does not remove valid already verified files.

## Decision and report privacy

Human culling state remains the input to delivery. Studio Brain recommendation rows are not read
as a final-selection rule. A plan-local include/exclude is a separate reversible catalog record;
it cannot modify `media_decisions`, decision history, notes, ratings, stars, representatives, or
source media.

The local JSON/text Delivery Report contains only delivery status facts: a non-internal delivery
reference, plan name, completion time, counts, verified bytes, manifest checksum, local-folder
destination type, and verification
policy. It intentionally omits notes, source or destination paths, internal asset/database IDs,
technical/AI scores, Studio predictions, embeddings, model data, and source-file lists.
