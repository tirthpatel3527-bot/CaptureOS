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

## Milestone 7 Moment Brain and Shoot Timeline Intelligence I rules

- Moment grouping is project-local structural organization from local evidence, not truth about an event, relationship, scene, intent, or what a photographer should have captured.
- Keep semantic retrieval, Similar Sets, and structural Moment grouping distinct. A Moment analysis must never create, rewrite, merge, or delete Similar Set membership, and Find Similar remains asset-to-asset retrieval.
- Never upload originals, managed previews, timeline metadata, labels, embeddings, face evidence, checklists, query text, or filesystem paths. Moment Brain must remain local, offline-capable, and free of paid/cloud API requirements.
- A local embedding score is not an object detector, caption, identity, event classifier, or proof that a concept is present. Suggested labels must be conservative, evidence-grounded, and may abstain as `Untitled Moment`.
- Never infer or label a bride, groom, person identity, family relationship, emotion, ceremony stage, wedding event, creative intent, or a required/missing shot. User-authored checklist/project terminology may be matched only as a user-supplied candidate, never invented.
- Persist AI suggested labels, their model/method/version/evidence, and human labels separately. A human rename controls presentation and never overwrites historical AI evidence.
- Suggested representatives are advisory and must expose only the local factors actually used. Never call one photo artistically best or use Moment evidence to alter Keep/Reject/Review, ratings, notes, flags, review sessions, or source media.
- Preserve append-only Moment events and human split, merge, rename, representative, and coverage-confirmation overrides across incremental work and rebuilds. Do not silently reset, reinterpret, or erase human organization.
- A Coverage Map may report observed asset counts, capture-time ranges/gaps, technical distributions, anonymous face-count availability, and human checklist state. The photographer-defined checklist is authoritative for expected coverage; Moment/Search evidence may never fabricate missing coverage or mark an item complete automatically.
- A camera-clock diagnostic is advisory only. Never rewrite EXIF, timestamps, source metadata, or catalog capture times as an automatic correction.
- Moment analysis must use durable background work, bounded chronological processing, and project-scoped persistence. It must never block startup, project opening, grid browsing, culling, Magic Search, or existing intelligence queues.
- Missing timestamps, incompatible embeddings, unavailable semantic model/provider, and offline originals must be represented honestly. Cached durable evidence may be used locally; an unavailable input must not halt unrelated assets.
- Treat timeline records, centroids, labels, boundary evidence, and index artifacts as sensitive local derived data. They must be rebuildable and never be the sole source of project identity or human decisions.
- Limit Milestone 7 to current-project still-photo Moment Brain, timeline organization, conservative labels, representatives, coverage checklist support, and advisory multicamera diagnostics. Do not add Milestone 8 work such as People Brain, personal training, event intelligence, creative/emotional judgment, video/audio understanding, cloud, collaboration, billing, write-back, deletion, or automatic culling.
- Milestone 8 is separately approved below. Do not begin Milestone 9 without explicit approval.

## Milestone 8 Studio Brain I rules

- Studio Brain is local, explicit, advisory preference modeling. Never treat an AI recommendation, passive behavior, indexing, search, hover, view time, zoom, or an unanswered suggestion as a human training label.
- Train only from explicit human decisions and approved explicit representative signals with immutable source/provenance records. Never use private notes, filenames, paths, raw embeddings, face identity, protected traits, emotion, attractiveness, client identity, or invented artistic/psychological claims as training input.
- A Studio recommendation never changes Keep/Reject/Review, ratings, stars, notes, representatives, Moment organization, coverage state, source metadata, original media, or exports. `REJECT` remains non-destructive CaptureOS metadata.
- Preserve generic Capture Intelligence and personalized Studio recommendations separately. If personalization is disabled, unavailable, stale, corrupt, or not ready, preserve the generic M0–M7 workflow and state the limitation honestly.
- Project opt-out governs contribution, not use of an already active local profile: do not materialize or train new examples from an excluded project, and never delete its human decisions to implement opt-out. Support decision-level exclusion without inferring an exception from a note.
- Use compact, static, versioned local artifacts only. Validate feature-schema compatibility and checksum before storage/activation; never deserialize arbitrary code, pickle, model hooks, Python payloads, or network resources.
- A candidate model must pass the documented readiness/evaluation gates, persist safely, and validate before atomic activation. Keep the previous valid model until activation succeeds; failure, interruption, corruption, or reset must leave human decisions untouched and fall back safely.
- Keep training/evaluation leakage-aware: use whole-project holdout when possible and otherwise hold Similar Set, Moment, or capture-day buckets together. Do not call within-snapshot agreement “accuracy” or claim generalization without a valid holdout.
- Confidence bands and explanations must come from actual available features/calibration. Prefer `NOT ENOUGH EVIDENCE` or `REVIEW` to a fabricated personalized conclusion.
- Never upload preference examples, decisions, ratings, stars, models, metrics, features, cached previews, embeddings, project names, paths, notes, or media. Studio Brain must remain offline-capable and free of mandatory cloud, telemetry, or paid APIs.
- Limit Milestone 8 to current photographer-controlled culling recommendations, explicit local training, local model lifecycle, Smart Cull/Similar Set advisory surfaces, and synthetic evaluation. Do not start Milestone 9, auto-culling, deletion, People Brain, event intelligence, editing/style training, video/audio intelligence, cloud, collaboration, billing, or write-back.
