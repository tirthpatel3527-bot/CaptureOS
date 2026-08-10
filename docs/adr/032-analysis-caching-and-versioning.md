# ADR 032: Analysis caching and versioned provenance

## Decision

Capture Intelligence caches current deterministic work by input/preview fingerprint, provider, provider version, and settings version. Its cache key is a BLAKE3 digest of those components. `AnalysisInputResolver` selects/reuses a source-fingerprint-matched 2048px-target `analysis_preview` before a sufficient 1600px browsing preview; otherwise it creates that contained cache artifact from an available FileInstance. Face/eye evidence has its own BLAKE3 input identity derived from the resolved preview input, face-provider identity, and face settings, so a local platform-capability revision can stale only face evidence. `AnalysisArtifact` keeps the full provenance/status/error tuple, and prior records with a changed input/provider/settings version are marked stale rather than destructively overwritten.

Background jobs persist progress, resource-mode choice, terminal counts, pause state, and restart recovery. Ready work is not regenerated merely because a view renders; corrupt, unsupported, needs-original, failed, and not-applicable outcomes are durable per-asset outcomes.

## Consequences

UI interaction does not create analysis churn, and offline originals can retain valid cached evidence. Revision-keyed fingerprints, embeddings, technical evidence, recommendations, and human-decision links preserve historical evidence during a transparent rebuild. One failed provider does not stall a project queue or erase a successful independent artifact.
