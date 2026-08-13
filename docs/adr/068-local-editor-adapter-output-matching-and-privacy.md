# ADR 068: Local editor adapters are generic, non-mutating, and privacy-bounded

## Decision

Milestone 10 permits only a generic local editor/output adapter boundary. An adapter may consume
an explicit, bounded Edit Session handoff and observe files beneath a photographer-selected local
output root. It must not read, create, modify, migrate, synchronize, or depend on a proprietary
editor catalog/database, and it must not drive an external editor UI, run vendor automation, or
write source metadata, XMP, sidecars, originals, or CaptureOS culling state.

The adapter has no authority to make a match from an unsafe heuristic. It validates selected roots
and relative paths, rejects traversal and unsafe links, records only actual local evidence, and
leaves uncertain results unmatched or ambiguous for the photographer to resolve. It does not use
face identity, demographic inference, emotion, raw embedding similarity, private note text, or a
proprietary catalog as a hidden output-matching signal.

All session manifests, output observations, match evidence, approval events, file fingerprints,
paths, labels, and any readable output metadata are sensitive local data. They remain offline,
are never telemetry or automatic export material, and are omitted from client-facing reports by
default. An adapter must work without cloud credentials, accounts, paid APIs, or a network call.

## Consequences

The initial boundary works with a photographer-controlled filesystem round trip without coupling
core domain logic to a particular editor or claiming integration where none exists. A future
adapter can be considered only through a separate approval, security/privacy review, and adapter
contract that preserves these no-mutation, conservative-matching, and local-only guarantees.

M10 remains an output-provenance and human-review foundation. It does not render, develop,
retouch, grade, transcode, delete, auto-approve, auto-deliver, or automatically import work from
an external editor.
