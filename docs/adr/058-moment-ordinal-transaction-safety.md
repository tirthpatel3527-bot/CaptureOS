# ADR 058: Transaction-safe Moment ordinal resequencing

## Decision

`moment_records` retains its unconditional `UNIQUE(run_id, ordinal)` invariant. Stale historical
rows remain part of that namespace; the invariant is not weakened or removed.

Manual merge and split actions continue to store human structural intent separately through
asset-anchored override operations and append-only events. Before a manual structural transaction
commits, CaptureOS resequences the complete active project projection in two collision-free
phases for each affected run:

1. move every record in that run to a checked temporary ordinal range above both the current and
   final ordinal ranges;
2. assign active records their explicit, project-wide display ordinal, then place stale rows
   after that run's active records in deterministic prior-ordinal/ID order.

This matters for an incremental tail: the active projection may intentionally contain an older
preserved prefix at `0..k` and a newer tail run beginning at `k + 1`. A merge or split in the
older prefix shifts every later active run in the same transaction, so the global display remains
contiguous rather than leaving `0, 2, 3` or colliding with the tail.

The split row is first staged at a non-conflicting temporary ordinal and participates in the same
resequence. This avoids transient constraint violations even when stale rows occupy an ordinal
that an active split must reclaim. Merge, split, their override/event writes, and any coverage
confirmation remapping are one SQLite `BEGIN IMMEDIATE` transaction; an error rolls back the
entire action. Human label, representative, and coverage-confirmation writes likewise record
their associated event in one transaction. Full and incremental replacement transactions remap a
coverage confirmation by its selected asset (or, when no asset was selected, its prior Moment
anchor), rebind a human representative to the active Moment containing the selected asset, and
move human labels by their durable anchor asset. A merge or generated resegmentation that would
converge distinct human labels fails closed rather than choosing a name. Incoming full and tail
payloads are checked for contiguous ordinals and against the complete active projection before
any new row is inserted. Mutable tail-replacement validation is performed after `BEGIN
IMMEDIATE`, not before it.

Rebuild and incremental update already create a distinct analysis run and atomically replace or
tail-swap the derived projection. Their prior valid timeline remains active if the transaction
fails. Desktop commands also serialize one Moment analysis or structural mutation per project;
the UI mirrors this with synchronous in-flight guards, but the backend remains authoritative.
An adjacent pair that spans two immutable incremental runs is intentionally not merged in place:
the UI and repository fail closed with an instruction to use **Rebuild AI timeline**, which is the
existing safe path to materialize one complete successor run while preserving separate human
overrides.

## Consequences

Active Moment cards always have project-wide unique, contiguous, deterministic ordinals after a
supported merge or split. Historical rows remain available locally without blocking a later
structural action, and human labels, representatives, merges, splits, and coverage confirmations
remain separate from generated analysis evidence.

This is deliberately a Milestone 7.2 persistence repair. It does not alter source media,
Similar Sets, Smart Cull decisions/history, semantic embeddings/indexes, project startup behavior,
or any Milestone 8 scope.
