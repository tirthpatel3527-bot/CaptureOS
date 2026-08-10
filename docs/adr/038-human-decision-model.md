# ADR 038: Human decision model

## Decision

Store current culling state in a project-scoped `media_decisions` row and every mutation in immutable `decision_history`/`review_events` rows. A decision is `KEEP`, `REVIEW`, or `REJECT`; no persisted row is required for `UNREVIEWED`. Rating (0–5), star, note, and flags are independent fields.

## Consequences

Human decisions remain fast to filter at catalog scale while their history survives restart. `REJECT` is only CaptureOS metadata and cannot touch originals, XMP, sidecars, or physical `FileInstance` copies. A human decision never updates or deletes an AI recommendation.
