# ADR 003: SQLite persistence

## Decision

Use versioned SQLite migrations behind repository interfaces.

## Consequences

Catalog data stays local and is queryable without loading a whole catalog in memory. Foreign keys are enabled per connection. UI code does not contain SQL.
