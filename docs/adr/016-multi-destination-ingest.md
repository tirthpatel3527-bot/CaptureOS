# ADR 016: Independent destination records

## Decision

An `IngestJob` owns any number of source records, one master destination record, and zero or more backup destination records. Every source-file/destination pair is an independent `ingest_item` with its own copy state, verification state, hashes, error, and FileInstance references.

## Consequences

A master can be verified while a backup fails without collapsing the evidence into one boolean. Jobs, audit events, and per-destination records survive restart and support a later retry pass without duplicating verified logical assets.
