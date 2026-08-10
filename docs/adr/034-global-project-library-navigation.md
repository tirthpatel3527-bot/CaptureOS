# ADR 034: Global Project Library navigation is distinct from project workspaces

## Decision

CaptureOS starts at the global Home / Project Library route (`#/`). A project workspace is reached only through a stable `ProjectId` route (`#/project/:projectId` or `#/project/:projectId/ingest`); project display names are never route or identity keys.

The Project Library is a derived SQLite projection over existing project, media, root, volume, background-job, and ingest records. It does not duplicate or migrate project data into a new summary table. Each card reports project-scoped media and storage counts, a recent meaningful activity timestamp, and only the protection evidence actually recorded for that project.

Desktop background events include the project ID with their progress payload. The renderer ignores events whose project ID does not match the active workspace. Asset detail, similarity-group reads, and human intelligence decisions also require a project ID and are rejected at the core boundary if the asset belongs to another project.

## Consequences

Existing projects, media, index roots, preview cache records, ingest history, Capture Intelligence artifacts, and CaptureGuardian evidence remain in their current SQLite tables. No destructive database migration is required. Multiple projects can share a display name while remaining independently routable and scoped.

The global shell provides a predictable return path to Home, a lightweight project switcher, and project creation without a dashboard redesign. Browser Back returns through prior in-app routes rather than relying on implicit first-project selection. Future storage overview and settings may use the shell foundation without being mixed into a project workspace.
