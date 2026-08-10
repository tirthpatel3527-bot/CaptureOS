# ADR 012: Streaming, destination-only ingest copy model

## Decision

Ingest reads each regular source file with bounded Rust I/O and writes only beneath the canonical master or backup root explicitly chosen by the user. The default layout is `Project/01_SOURCES/Source Label/relative/path`. It is represented by the `DestinationLayout` trait so later template choices do not alter core copy logic.

## Consequences

Source folders remain untouched and source namespaces preserve duplicate filenames. Milestone 2 is deliberately sequential and deterministic; later scheduling/parallelism must preserve the same evidence and no-overwrite rules.
