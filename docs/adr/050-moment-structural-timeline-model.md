# ADR 050: Additive structural Moment timeline model

## Decision

Milestone 7 adds a project-scoped, versioned timeline analysis model for structural segments, Moments, memberships, boundary evidence, label/representative suggestions, and human override events. It is additive: it does not repurpose or reinterpret the Phase 0 **shoots** and **moments** rows or existing CaptureGraph **BelongsToMoment** edges. Those foundational records remain valid historical/domain data; M7 analysis records use an explicit timeline namespace and provenance identity.

A timeline analysis run records project scope, analyzer/boundary algorithm identity, compatible semantic-provider/model/preprocessing identity where present, input catalog fingerprint, terminal status, and timestamps. Migration 012 uses `shoot_timelines`, `moment_analysis_runs`, `timeline_segments`, `moment_records`, `moment_memberships`, and `moment_boundary_evidence` for that projection. Its current derived projection can be replaced only by a compatible successful rebuild. Historical runs/evidence remain provenance rather than being rewritten as if they were current.

Every M7 derived row is project-scoped and foreign-key-linked. `moment_human_labels`, `moment_human_representatives`, `moment_override_operations`, and append-only `moment_events` remain outside rebuildable derived rows. Membership is the authoritative scope for a Moment after a human merge or split; a start/end time range is presentation evidence only. Derived centroid/index material remains rebuildable from durable per-asset embeddings and never becomes the only source of project, asset, or human-decision identity.

## Consequences

Migration preserves existing projects, legacy shoots/moments, M0–M6 media, Similar Sets, Capture Intelligence evidence, Magic Search rows/indexes/history, and human culling data. A cross-project asset/Moment request is rejected at repository/core boundaries rather than relying on frontend state.

The explicit M7 namespace prevents a future product feature from treating a structural suggestion as a user-authored event fact. It also allows later, separately approved domain modeling to integrate old Shoot/Moment entities intentionally rather than accidentally changing them during analysis.
