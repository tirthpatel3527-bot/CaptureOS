# ADR 062: Studio Brain generated benchmark and preference-privacy boundary

## Decision

Milestone 8 adds `research/studio-brain-bench`, a deterministic in-memory benchmark at 100, 1k,
10k, 50k, and 100k synthetic observations. It measures generated-data training/inference
mechanics, artifact size, grouped holdout baselines, calibration/abstention output, pairwise
ranking availability, and divergent controlled Studio A/Studio B preferences. It performs no
network request and opens no catalog, AI Test data, media, preview, embedding, or model weight.

Preference examples, profile settings, exclusions, compact snapshots, metrics, model artifacts,
and recommendations are sensitive local derived data. They are not telemetry or automatic export,
and the product never uploads them. Any real-photographer research requires explicit opt-in and a
separately approved consent, privacy, licensing, fixture, and evaluation plan.

## Consequences

The benchmark can expose scale or deterministic-recovery regressions without presenting synthetic
labels as real artistic truth, photographer accuracy, or authorization for automatic culling.
Private decisions, notes, names, paths, media, raw embeddings, and real profile artifacts must
never be committed to the repository. Project deletion retention policy remains explicitly
undecided until a project-deletion feature is approved.
