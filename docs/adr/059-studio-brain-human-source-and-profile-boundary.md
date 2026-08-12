# ADR 059: Studio Brain explicit human-source and profile boundary

## Decision

Milestone 8 introduces a profile-scoped local `StudioProfile` with a single `local-default`
profile in the UI. Studio training source rows are immutable, provenance-bearing records created
only from intentional human culling actions, explicit M4 overrides, and approved human
representative selections. They retain project/asset/source IDs, action type/value/time, review
context where known, Similar Set/Moment context, generic/Studio recommendation presentation
provenance, eligibility, and a compact feature snapshot.

AI recommendations, passive interaction, search/view time, indexing, and unanswered advice are
not sources. Notes, filenames, paths, raw embeddings, identity/demographic fields, and source
media are never training features. Rating/star actions remain explicit auxiliary source records;
they are not silently relabeled as Keep/Reject/Review.

Project participation defaults on but an explicit opt-out prevents both live materialization and
historical backfill while preserving all normal human decisions. A decision-level exclusion is a
separate local record and must not be inferred from note text. Historical backfill is idempotent,
explicitly requested by Train/Update, and keeps legacy recommendation visibility `unknown`.

## Consequences

Profile-scoped foreign keys make multiple future local profiles possible without adding teams or
accounts. A human decision remains authoritative and Studio advice cannot self-train. An opt-out
controls contribution, not whether a current active local model may be used to show advisory
recommendations. Existing M5 history is never rewritten or deleted.
