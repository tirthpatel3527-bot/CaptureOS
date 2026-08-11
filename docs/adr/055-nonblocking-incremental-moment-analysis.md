# ADR 055: Private, nonblocking, project-scoped incremental Moment analysis

## Decision

Moment analysis is an explicit durable project-scoped local background operation with resource mode, progress, pause/resume, per-item/range errors, and terminal state. Timeline inputs, boundary evidence, labels, centroids, checklists, diagnostics, and human overrides stay in the local project catalog; no original, preview, metadata, embedding, path, or timeline record is uploaded, telemetered, or automatically exported. Opening CaptureOS, Project Home, a grid, Magic Search, Smart Cull, or an existing Capture Intelligence view reads only bounded status/current projections; it never implicitly starts a model load, scan, rebuild, or timeline analysis.

An explicit Update reevaluates a bounded chronological tail/window around new compatible input. The repository obtains the small preceding context and append batch, then calls the local `analyze_append_only_tail` API; it must not first sort the full catalog merely to call an update incremental. The full-input validator is retained only as a conservative compatibility/fallback path. An out-of-order timestamp, changed input fingerprint, incompatible semantic identity, changed analyzer configuration, incomplete append declaration, or missing protected context may conservatively require a rebuild. Rebuild replaces only derived analysis projection rows, preserves historical provenance, and re-applies human override anchors.

## Consequences

Moment Brain cannot hold project startup hostage or starve other local queues. An unavailable source, preview, embedding, or semantic provider remains an honest terminal/partial state and does not block unrelated assets or projects. Current compatible cached metadata/embeddings can support offline work without requiring an original volume. Derived timeline records are rebuildable local evidence, never the sole source of a project or human decision.

Project isolation is enforced before job creation, input resolution, timeline read, Moment detail, culling scope, or Magic Search scope. M7 performs no cross-project timeline/person correlation, remote processing, automatic data export, or M8 work.
