# ADR 047: Hybrid Magic Search query planning

## Decision

Magic Search routes a current-project query through a modest `QueryPlanner`. It extracts only
high-confidence deterministic predicates already supported by persisted evidence: face count,
rating, human decision, technical sharpness/blur/issue evidence, and camera model. The remaining
ordinary-language phrase goes to the local text embedding provider only when an admitted semantic
model is available. Unknown language falls back to semantic retrieval rather than a large,
fragile hand-written grammar.

`MetadataSearchProvider` and `SemanticEmbeddingProvider` supply candidates; `HybridRanker` ranks
only signals in the plan. Result explanations enumerate the actual applied structured evidence.
A result based only on vector similarity says “Strong semantic match”; it does not claim an object
detection, object location, person identity, or probability. Search is read-only: it never alters
ratings, decisions, notes, representatives, sessions, Similar Sets, or originals.

## Consequences

Queries such as `5 star photos with 1 face` can use deterministic evidence without pretending a
model understood every token. A query such as `yellow boat` may use a real local semantic vector
when available, but absence of the model produces an explicit unavailable state while deterministic
filters still work. Raw vector distances stay in developer diagnostics, not normal photographer
UX.

Find Similar is a distinct image-to-image query path over the same index. It must not merge with
or mutate the M4 `SimilarityGroup` algorithm, whose purpose remains conservative sequence/burst
grouping.
