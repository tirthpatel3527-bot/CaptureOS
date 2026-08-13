# ADR 067: Derived outputs retain versioned provenance and explicit human approval

## Decision

An externally produced local output is never treated as a replacement for its source. When
CaptureOS records one, it preserves a separate logical-output/physical-file identity and a durable
provenance record containing the Edit Session, observed local file evidence, any source-link
evidence, version/revision relationship, timestamps, and matching state. `MediaAsset` and
`FileInstance` remain distinct for outputs just as they are for originals.

An exact or manual output link may be recorded only when the stored local evidence identifies one
source or session target unambiguously. A `strong` or `possible` candidate remains documented
matching evidence, not an implicit human decision; a filename, capture time, embedding score,
visual resemblance, or editor assumption alone is not authoritative. Missing or competing
evidence must remain `unmatched` or `ambiguous`; CaptureOS must not invent an editor recipe, edit
history, source relationship, or output version.

New observations and revisions preserve earlier output provenance rather than overwriting it.
Availability may change as a local output volume is connected or disconnected, but a later output
does not silently erase an earlier version or its source-link evidence. Output matching is a
catalog claim with its own provenance, not a mutation of the original source record.

Output approval is a separate explicit human action. An observed file, a successful match, an
M9 delivery selection, a generic recommendation, or an external application state never creates
approval automatically. Approval does not alter Keep/Reject/Review, ratings, stars, notes,
representatives, Moments, Studio Brain data, a Production Plan, or a delivery manifest. A later
delivery still requires a fresh photographer-controlled M9 plan and immutable manifest.

## Consequences

CaptureOS can show a restrained history of observed output versions and explain why an output is
linked without claiming to have performed the edit. Uncertain results remain useful local evidence
while clearly distinguished from a human-approved output. The model supports future approved
output workflows without making a current source file mutable, treating an edited derivative as
the original, or turning automatic matching into creative/delivery authority.
