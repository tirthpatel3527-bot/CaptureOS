# ADR 042: Culling keyboard workflow

## Decision

The Culling Workspace exposes a keyboard-first decision workflow: `K`, `X`, `R`, `S`, `1`–`5`, `0`, arrow navigation, Space, `C`, `F`, `G`, and `U`. It ignores shortcuts when focus is in input, textarea, select, or content-editable elements.

## Consequences

Rapid review remains accessible without a mouse and does not corrupt notes or filters. Auto Advance is local preference state and can be disabled; optimistic navigation is reconciled with durable background persistence and reports an error if saving fails.
