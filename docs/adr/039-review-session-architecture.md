# ADR 039: Review session architecture

## Decision

Create a local `ReviewSession` when the Culling Workspace is opened, retain its mode/filter and last asset/group position, and close it only on explicit Finish Review. Record only meaningful lifecycle/review events.

## Consequences

Photographers can resume a partial cull without persisting fragile frontend layout state or every hover/click. Session Undo/Redo changes durable decision metadata through new history entries; it never changes AI evidence.
