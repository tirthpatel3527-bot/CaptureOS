# ADR 063: Production Plan, Export Manifest, and Export Job are separate records

## Decision

Milestone 9 models local delivery in three layers:

- `ProductionPlan` is editable photographer intent: explicit human selection rules, optional
  plan-only overrides, Moment folder strategy, filename strategy, and a selected local
  destination.
- `ExportManifest` is a complete immutable snapshot of one validated plan. It persists selected
  `MediaAsset` and `FileInstance` identities, safe destination-relative names, selection and
  naming snapshots, estimates, validation facts, and a deterministic BLAKE3 checksum. It never
  stores a source absolute path in a manifest entry.
- `ExportJob` is one actual execution of a frozen manifest. It has an independent durable
  background job and per-entry outcome states.

Manifest creation runs in one immediate SQLite transaction. It rechecks the current plan
configuration and a project-scoped selection revision after the complete candidate is built; an
inserted human decision-history, Moment event, or static-collection membership update invalidates
a concurrent candidate rather than allowing an unintentional snapshot. A
partial unique index and a desktop process guard allow only one active job per manifest. A plan
cannot change destination, selection override, or configuration while its current manifest is
exporting.

## Consequences

Changing plan settings clears its current-manifest pointer and requires a fresh dry run. A
completed job updates plan state only when it still owns that current manifest, so a historical
job cannot overwrite a later plan state. Restarting, cancelling, or partially completing a job
does not rewrite the plan, source media, decision history, or prior manifest entries. The
workspace presents a bounded recent-history projection; it does not load an entire catalog or
manifest into the frontend.
