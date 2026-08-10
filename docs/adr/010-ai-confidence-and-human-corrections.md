# ADR 010: AI confidence and human corrections

## Decision

Inference records include confidence and provenance; corrections are append-only records rather than destructive overwrites.

## Consequences

The product can distinguish an algorithmic suggestion from a user-confirmed fact and can preserve history for future preference learning.
