# ADR 064: Virtual Collections and delivery selection remain human-controlled metadata

## Decision

`VirtualCollection` is a project-local logical group of `MediaAsset` references. A static
collection stores explicit membership references; a dynamic collection stores the same bounded
human-facing rule model used by production selection (decision, rating, star, Moment, and a
static asset subset). Neither kind duplicates, moves, hides, renames, or writes media.

Production selection starts from explicit human culling state. The Client Delivery template uses
`KEEP`; Editor Workset uses `KEEP` and `REVIEW`. Plan-local `force_include` and `force_exclude`
records are intentionally separate from `media_decisions`: they provide organization exceptions
without changing Keep/Reject/Review, rating, star, note, review history, or Moment records.
Studio Brain fields are absent from these schemas and are never evaluated as delivery truth.

A plan may reference one project-local collection. Its resolved members intersect the plan's
explicit human rules; a plan-only force include remains an intentional exception and force
exclude remains final for that plan. The desktop edits static membership one asset at a time from
a bounded inspection view, so it never loads a huge collection into frontend memory. Changing a
referenced static collection marks its current manifest/plan stale for explicit refresh and
advances the manifest selection revision in the same transaction. A dry run that began before
the membership action is rejected at freeze rather than silently freezing old members; no
immutable manifest is rewritten.

## Consequences

Collections and overrides are reversible local catalog metadata. A human can prepare a client
workset without converting an excluded-but-kept frame into Reject, and Studio Brain remains an
optional advisory surface only. Dynamic evaluation is project-scoped and deterministic; a
manifest freezes its resulting selection before any filesystem write. This leaves a future
editor/cloud adapter able to consume the same manifest without gaining authority over human
decisions.
