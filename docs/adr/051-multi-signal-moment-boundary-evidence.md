# ADR 051: Bounded multi-signal Moment boundary evidence

## Decision

Moment Brain derives structural boundaries from a bounded chronological stream of current-project still-photo evidence. It orders timestamped assets by capture time and a stable asset-ID tie-breaker. An asset without a trustworthy timestamp remains explicitly ungrouped/uncertain; the analyzer never invents a time or silently puts it in the nearest Moment.

The analyzer evaluates adjacent records and a small bounded local context, not all pairs of a catalog. Its versioned evidence may include:

- adaptive local capture cadence/gap derived from observed data;
- compatible local semantic embedding continuity or rolling-centroid distance;
- camera, lens, orientation, optional anonymous face-count transition, and Similar Set continuity as supporting evidence.

A semantic vector is used only when its model/provider/preprocessing/dimension identity is compatible. Missing or unavailable components are recorded and excluded/renormalized by the versioned algorithm; they do not become a false zero or confidence. Normal UI reports qualitative boundary bands and actual contributing evidence, not a probability or claim of event/object/person detection.

## Consequences

The implementation remains local, deterministic for a given evidence snapshot/configuration, and scales with bounded chronological comparisons rather than quadratic pairwise retrieval. It does not hard-code a wedding timeline, AI Test content, a universal timing threshold, or a product-specific event taxonomy.

A boundary is a navigation/review suggestion. It does not establish a real-world event, relationship, emotion, identity, photographer intent, or missing coverage. Similar Sets remain a separate conservative related-frame/burst system and are never rewritten by Moment analysis.
