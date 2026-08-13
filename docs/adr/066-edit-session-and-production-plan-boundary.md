# ADR 066: Edit Sessions are distinct from Production Plans

## Decision

Milestone 10 introduces an `EditSession` as an explicit, local record of a
photographer-directed editing handoff and output-review cycle. Each Edit Session is created from
one frozen M9 `ExportManifest`, but it is not a `ProductionPlan`, `ExportManifest`, or
`ExportJob`.

- A `ProductionPlan` remains editable delivery intent: it describes a human-controlled selection,
  organization, naming, and destination policy.
- An `ExportManifest` remains the immutable M9 snapshot used by a verified local-copy job.
- An `ExportJob` remains one execution of that immutable manifest.
- An `EditSession` records the deliberately chosen edit-workset context, its handoff snapshot,
  locally observed outputs, and explicit human review/approval actions.

Creating, changing, closing, or archiving an Edit Session must not rewrite a Production Plan,
manifest, export job, culling decision, rating, star, note, Similar Set representative, Moment
record, Studio recommendation, or source media. Conversely, an M9 plan or completed export does
not imply that an Edit Session exists, that an external output is matched, or that an output is
approved.

## Consequences

A photographer can use the same human selection for several independent edit rounds without
turning a delivery plan into an editor-state record. A session can retain a historical reference
to the handoff it used even when later Production Plan edits create newer manifests. The session
does not execute a plan, re-evaluate a frozen manifest, make a delivery decision, or become a
substitute for an editor's proprietary catalog.

Derived-output provenance and human approval are separate M10 records. A session can be active
with no outputs, and a locally observed output can remain unmatched or unapproved. This keeps
external editing context, source selection, output evidence, and delivery authority independently
auditable and reversible.
