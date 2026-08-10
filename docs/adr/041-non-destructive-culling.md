# ADR 041: Non-destructive culling

## Decision

Culling changes only the CaptureOS SQLite catalog. Reports are explicit user-selected exports created as new files, never source-folder write-back.

## Consequences

Keep/Reject/Review, rating, star, notes, flags, bulk operations, representatives, and completion are reversible metadata decisions. They do not delete, move, hide, rename, trash, format, copy, ingest, alter sidecars, or modify media bytes.
