# ADR 001: Local-first architecture

## Decision

CaptureOS core functions operate on local user-controlled storage and local SQLite. Cloud is neither required nor present in Phase 0.

## Consequences

Original media stays where the user placed it, offline operation is possible, and no storage or media-count entitlement is embedded in the design.
