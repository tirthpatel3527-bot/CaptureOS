# CaptureOS engineering guide

CaptureOS is local-first software for professional camera media. Preserve these non-negotiable rules:

- Never add mandatory cloud, authentication, hosted storage, telemetry, or a paid dependency without explicit approval.
- Never add artificial media, project, catalog, or storage limits.
- Never destructively alter, overwrite, rename, delete, or format original media. Never format cards.
- Never fake AI results, camera integrations, media analysis, or vendor/cloud services.
- Keep `MediaAsset` and its physical `FileInstance` copies separate.
- Maintain the adapter boundaries; do not hard-code a specific camera, editor, AI model, platform, or storage vendor into core domain logic.
- Treat all paths, filenames, metadata, volumes, sidecars, and media as untrusted. Do not shell out with unsanitized paths.
- Do not expose secrets or add a real `.env` file.
- Add focused tests for changed core behavior and preserve foreign-key integrity.
- Document material architecture choices in `docs/adr/` and update the system diagram where useful.
- Do not turn the Phase 0 checkpoint into a generic dashboard or fake final product.
- Stop at the requested milestone. Propose later work; do not silently implement it.
- Never mark an unverified copy as verified, and never bypass verification to report success.
- Never claim redundancy from copies on the same physical StorageVolume without a clear warning.
- Never format or modify source media during ingest, including by writing temporary files or sidecars.
- Never silently overwrite destination content; report a collision or retain a marked partial instead.
- Never decode full originals repeatedly for grid browsing; use bounded metadata and CaptureOS-managed preview artifacts.
- Never write previews, cache files, temporary renders, or metadata beside customer originals.
- Never alter source metadata or claim an unsupported RAW/video decoder works.
- Never load an entire huge catalog into frontend memory; use repository pagination/windowing.
- Never add AI analysis, semantic results, or mandatory cloud work before the explicitly approved milestone.

## Milestone 4 Capture Intelligence rules

- Never fake AI outputs, confidence, face counts, eye states, technical scores, or availability states. When a local provider cannot make a valid claim, preserve and show its uncertainty or unavailable status.
- Never delete, hide permanently, move to Trash, or otherwise cull media based on AI evidence or a recommendation.
- Never upload customer media, face crops, embeddings, metadata, or analysis artifacts for intelligence processing without explicit future approval.
- Never present technical quality as a universal artistic-quality judgment. Keep technical evidence and creative preference separate.
- Always preserve analysis confidence, provider/model provenance, input fingerprint, settings version, status, and error information. Mark superseded evidence stale; do not overwrite history to make it look current.
- A human correction overrides the currently displayed recommendation but does not erase the original AI recommendation or its provenance.
- One analyzer, provider, corrupt file, or unsupported file failure must not block the rest of an analysis queue.
- Do not add person identity recognition, cross-project face matching, demographic inference, or biometric profiles without explicit future approval.
- Respect model, provider, dataset, and tool licenses. Do not bundle, auto-download, execute, or represent an external model as approved until its commercial redistribution terms and integrity have been reviewed.
- Stop at the requested milestone. Capture Intelligence I does not authorize Milestone 5 features such as semantic search, People Brain, video/audio intelligence, cloud, or automatic culling.

## Milestone 5 Culling and Studio Brain foundation rules

- AI recommendation is never a human decision. Preserve both the advisory evidence and the photographer's current decision/history.
- `REJECT` is CaptureOS metadata only: never delete, hide, move, rename, trash, format, or modify original media because of it.
- Never write source XMP, sidecars, ratings, flags, or notes during culling. CaptureOS's local catalog remains the source of truth until an explicitly approved export milestone.
- Preserve append-only decision history and lightweight review events; do not rewrite the past to make current state look like it was always selected.
- Human preference examples, review sessions, reports, and AI/Human agreement remain local. Never upload them or train Studio Brain until explicitly approved.
- Do not call AI/Human agreement “accuracy” without independently established ground truth.
- Do not fake creative, emotional, identity, or aesthetic reasoning. Culling presents technical evidence and photographer-controlled decisions only.
- Stop at the requested milestone. Milestone 5 does not authorize personalized training, semantic search, People/Moment Brain, write-back, deletion, cloud, video/audio intelligence, or collaboration.

## Milestone 6 Magic Search and local visual understanding rules

- Never upload customer media, cached previews, search text, embeddings, face information, metadata, or filesystem paths for semantic search. Magic Search remains local and offline-capable.
- Never fake a semantic match, semantic relevance, embedding, model availability, object detection, or query result. If a local semantic provider is unavailable, preserve metadata and technical filtering and say that semantic search is unavailable.
- Never describe embedding similarity as an object detector, localized object claim, identity claim, or proof that a concept is present. Use evidence-based language such as “Strong semantic match” unless a separate approved detector produced the stated evidence.
- Never bundle, auto-download, execute, or call an unclear-license model weight, tokenizer, conversion script, Python/pickle payload, or model-supplied executable hook. A semantic model pack must be explicit, locally installed, checksum-validated where configured, static, and admitted through the local model registry.
- Keep Magic Search semantic similarity separate from Similar Sets. Similar Sets remain conservative related-frame/burst grouping; semantic nearest-neighbor retrieval must not create or rewrite Similar Set membership.
- Keep human decisions, ratings, notes, representatives, review sessions, and their history unchanged during search. Search is a read-only retrieval and navigation workflow.
- Do not add person identity recognition, face matching, person clustering, demographic inference, cross-project person search, or biometric profiles. Face count is permitted only as existing anonymous evidence.
- Treat embeddings and semantic-index files as potentially sensitive derived data. Store them locally, never export them automatically, and make the index rebuildable from durable embedding records; it is never the sole source of project identity or human decisions.
- Limit Milestone 6 to current-project still-photo Magic Search, deterministic/hybrid filters, and Find Similar. Do not add People Brain, Moment Brain, creative/emotional judgment, video/audio semantic search, cloud, collaboration, billing, or other later milestones.
- Stop at the requested milestone. Do not begin Milestone 7 without explicit approval.
