# ADR 052: Append-only human Moment organization overrides

## Decision

Moment Brain stores meaningful human organization actions as append-only project-scoped events: **MOMENT_CREATED**, **MOMENT_RENAMED**, **MOMENT_MERGED**, **MOMENT_SPLIT**, **MOMENT_REPRESENTATIVE_CHANGED**, and **COVERAGE_CONFIRMED**. Indexed current-state projections make rendering fast, but the event log preserves the evidence that a photographer changed the derived organization.

Human rename/representative records are separate from AI label/representative suggestions. Merge and split anchors reference durable project asset IDs and memberships, not an ordinal or a mutable time range. A new analysis run may replace compatible derived segments, then overlays protected override anchors; it must not erase, silently reset, or transform a human action.

## Consequences

A photographer’s organization persists across restart, incremental updates, model/version invalidation, and rebuilds without claiming that the current derived run always existed. The UI can clearly distinguish a suggested label/representative from a human-selected one.

M7 events do not replace M5 **decision_history**, **review_events**, review sessions, ratings, notes, or Similar Set human representatives. Moment analysis and its human organization tools never alter source files, CaptureGraph Similar Set membership, Keep/Reject/Review state, or their history.
