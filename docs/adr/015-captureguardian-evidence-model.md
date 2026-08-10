# ADR 015: CaptureGuardian is evidence-derived

## Decision

CaptureGuardian is computed from persisted ingest-item verification states and destination `StorageVolume` identities, never from folder count or successful copy calls. Basic means a verified master (`MASTER_COPY`); Standard requires a verified master plus verified backup on independent volumes (`PROTECTED`). Failures, conflicts, source changes, or unavailable paths produce `NEEDS_ATTENTION`.

## Consequences

Two destinations on one volume can be verified but are only `PARTIALLY_PROTECTED` under Standard, with an explicit warning. Safe-to-eject is a conservative workflow statement and never ejects or formats media.
