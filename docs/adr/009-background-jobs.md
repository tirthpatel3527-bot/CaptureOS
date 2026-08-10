# ADR 009: Background jobs

## Decision

Model job state and stages now, before implementing processing.

## Consequences

Large catalog work can later be resumable, observable, pausable, and cancellable without reshaping the core schema.
